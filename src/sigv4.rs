//! Подпись запросов к S3-совместимому хранилищу по AWS Signature Version 4.
//!
//! SHA-256 и HMAC берутся из CommonCrypto, так что ни SDK, ни крипто-библиотеки не нужны.

use std::ffi::c_void;
use std::fmt::Write;

/// Тело, которое не подписывается: так подписаны ссылки с ограниченным сроком.
pub const UNSIGNED_PAYLOAD: &str = "UNSIGNED-PAYLOAD";

unsafe extern "C" {
    fn CC_SHA256(data: *const c_void, len: u32, md: *mut u8) -> *mut u8;
    fn CCHmac(algorithm: u32, key: *const c_void, key_length: usize, data: *const c_void, length: usize, out: *mut u8);
}

/// kCCHmacAlgSHA256.
const HMAC_SHA256: u32 = 2;

pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut digest = [0u8; 32];
    // SAFETY: буфер на 32 байта - ровно размер SHA-256, данные живут до конца вызова.
    unsafe { CC_SHA256(data.as_ptr().cast(), data.len() as u32, digest.as_mut_ptr()) };
    digest
}

fn hmac(key: &[u8], data: &[u8]) -> [u8; 32] {
    let mut out = [0u8; 32];
    // SAFETY: ключ и данные живут до конца вызова, выход - 32 байта HMAC-SHA256.
    unsafe { CCHmac(HMAC_SHA256, key.as_ptr().cast(), key.len(), data.as_ptr().cast(), data.len(), out.as_mut_ptr()) };
    out
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Кодирование URI по правилам S3: всё, кроме букв, цифр и `-._~`, а для пути ещё и `/`.
pub fn encode(text: &str, keep_slash: bool) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        let plain = byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) || (keep_slash && byte == b'/');
        if plain {
            out.push(byte as char);
        } else {
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Ключи доступа и область подписи.
pub struct Credentials<'a> {
    pub access_key: &'a str,
    pub secret_key: &'a str,
    pub region: &'a str,
}

/// Время запроса: `YYYYMMDDTHHMMSSZ`.
#[derive(Clone, Copy)]
pub struct Stamp<'a>(pub &'a str);

impl Stamp<'_> {
    fn date(&self) -> &str {
        &self.0[..8]
    }
}

/// Время в формате подписи из секунд Unix.
pub fn stamp(unix: u64) -> String {
    let days = (unix / 86_400) as i64;
    let seconds = unix % 86_400;
    // Дни от 1970-01-01 в дату, алгоритм Говарда Хиннанта.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z", seconds / 3600, seconds / 60 % 60, seconds % 60)
}

fn scope(credentials: &Credentials, stamp: Stamp) -> String {
    format!("{}/{}/s3/aws4_request", stamp.date(), credentials.region)
}

/// Подпись канонического запроса. `headers` - имена в нижнем регистре и значения,
/// `query` - параметры без кодирования, `path` - путь без кодирования.
fn signature(
    credentials: &Credentials,
    stamp: Stamp,
    method: &str,
    path: &str,
    query: &[(&str, String)],
    headers: &[(&str, String)],
    payload_hash: &str,
) -> (String, String) {
    let mut query: Vec<(String, String)> = query.iter().map(|(k, v)| (encode(k, false), encode(v, false))).collect();
    query.sort();
    let query = query.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
    let mut headers: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.trim())).collect();
    headers.sort();
    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let signed_headers = headers.iter().map(|(k, _)| *k).collect::<Vec<_>>().join(";");
    let canonical =
        format!("{method}\n{}\n{query}\n{canonical_headers}\n{signed_headers}\n{payload_hash}", encode(path, true));
    let to_sign =
        format!("AWS4-HMAC-SHA256\n{}\n{}\n{}", stamp.0, scope(credentials, stamp), hex(&sha256(canonical.as_bytes())));
    let key = [stamp.date(), credentials.region, "s3", "aws4_request"]
        .iter()
        .fold(format!("AWS4{}", credentials.secret_key).into_bytes(), |key, part| hmac(&key, part.as_bytes()).to_vec());
    (hex(&hmac(&key, to_sign.as_bytes())), signed_headers)
}

/// Заголовок Authorization для запроса с подписанными заголовками.
pub fn authorization(
    credentials: &Credentials,
    stamp: Stamp,
    method: &str,
    path: &str,
    headers: &[(&str, String)],
    payload_hash: &str,
) -> String {
    let (signature, signed_headers) = signature(credentials, stamp, method, path, &[], headers, payload_hash);
    format!(
        "AWS4-HMAC-SHA256 Credential={}/{},SignedHeaders={signed_headers},Signature={signature}",
        credentials.access_key,
        scope(credentials, stamp)
    )
}

/// Строка запроса ссылки на GET, которая работает `expires` секунд. Подписан только заголовок host.
pub fn presigned_query(credentials: &Credentials, stamp: Stamp, host: &str, path: &str, expires: u64) -> String {
    let credential = format!("{}/{}", credentials.access_key, scope(credentials, stamp));
    let query = [
        ("X-Amz-Algorithm", "AWS4-HMAC-SHA256".to_string()),
        ("X-Amz-Credential", credential),
        ("X-Amz-Date", stamp.0.to_string()),
        ("X-Amz-Expires", expires.to_string()),
        ("X-Amz-SignedHeaders", "host".to_string()),
    ];
    let (signature, _) =
        signature(credentials, stamp, "GET", path, &query, &[("host", host.to_string())], UNSIGNED_PAYLOAD);
    let mut pairs: Vec<String> = query.iter().map(|(k, v)| format!("{k}={}", encode(v, false))).collect();
    pairs.push(format!("X-Amz-Signature={signature}"));
    pairs.join("&")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Примеры из документации Amazon S3 «Signature Calculations for the Authorization Header»
    // и «Authenticating Requests: Using Query Parameters».
    const EXAMPLE: Credentials = Credentials {
        access_key: "AKIAIOSFODNN7EXAMPLE",
        secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        region: "us-east-1",
    };
    const HOST: &str = "examplebucket.s3.amazonaws.com";
    const STAMP: Stamp = Stamp("20130524T000000Z");
    const EMPTY_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn get_object_matches_the_aws_example() {
        let headers = [
            ("host", HOST.to_string()),
            ("range", "bytes=0-9".to_string()),
            ("x-amz-content-sha256", EMPTY_HASH.to_string()),
            ("x-amz-date", STAMP.0.to_string()),
        ];
        assert_eq!(
            authorization(&EXAMPLE, STAMP, "GET", "/test.txt", &headers, EMPTY_HASH),
            "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/us-east-1/s3/aws4_request,\
             SignedHeaders=host;range;x-amz-content-sha256;x-amz-date,\
             Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        );
    }

    #[test]
    fn put_object_matches_the_aws_example() {
        let body = hex(&sha256(b"Welcome to Amazon S3."));
        assert_eq!(body, "44ce7dd67c959e0d3524ffac1771dfbba87d2b6b4b4e99e42034a8b803f8b072");
        let headers = [
            ("date", "Fri, 24 May 2013 00:00:00 GMT".to_string()),
            ("host", HOST.to_string()),
            ("x-amz-content-sha256", body.clone()),
            ("x-amz-date", STAMP.0.to_string()),
            ("x-amz-storage-class", "REDUCED_REDUNDANCY".to_string()),
        ];
        let header = authorization(&EXAMPLE, STAMP, "PUT", "/test$file.text", &headers, &body);
        assert!(
            header.ends_with("Signature=98ad721746da40c64f1a55b78f14c238d841ea1380cd77a1b5971af0ece108bd"),
            "{header}"
        );
    }

    #[test]
    fn presigned_url_matches_the_aws_example() {
        let query = presigned_query(&EXAMPLE, STAMP, HOST, "/test.txt", 86_400);
        assert_eq!(
            query,
            "X-Amz-Algorithm=AWS4-HMAC-SHA256\
             &X-Amz-Credential=AKIAIOSFODNN7EXAMPLE%2F20130524%2Fus-east-1%2Fs3%2Faws4_request\
             &X-Amz-Date=20130524T000000Z&X-Amz-Expires=86400&X-Amz-SignedHeaders=host\
             &X-Amz-Signature=aeeed9bbccd4d02ee5c0109b86d86835f995330da4c265957d157751f604d404"
        );
    }

    #[test]
    fn unix_time_becomes_the_signing_stamp() {
        assert_eq!(stamp(1_369_353_600), "20130524T000000Z");
        assert_eq!(stamp(0), "19700101T000000Z");
        // 29 февраля високосного года и конец дня
        assert_eq!(stamp(1_709_251_199), "20240229T235959Z");
    }

    #[test]
    fn keys_are_encoded_like_s3_expects() {
        assert_eq!(
            encode("Kadro 2026/Снимок 1.png", true),
            "Kadro%202026/%D0%A1%D0%BD%D0%B8%D0%BC%D0%BE%D0%BA%201.png"
        );
        assert_eq!(encode("a/b", false), "a%2Fb");
    }
}
