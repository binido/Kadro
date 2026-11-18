//! Uploading screenshots to S3-compatible storage and putting their link on the clipboard.
//!
//! Requests are signed in [`sigv4`] and sent through URLSession. Access keys are stored
//! in the Keychain, other storage settings in `NSUserDefaults`.

use std::ffi::c_void;
use std::sync::mpsc;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::MainThreadMarker;
use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication, NSPasteboard, NSPasteboardTypeString, NSScreen};
use objc2_core_foundation::{CFBoolean, CFData, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_graphics::CGImage;
use objc2_foundation::{
    NSData, NSError, NSHTTPURLResponse, NSMutableURLRequest, NSString, NSURL, NSURLResponse, NSURLSession, ns_string,
};

use crate::capture::Shot;
use crate::settings::{self, LinkExpiry, LinkKind};
use crate::sigv4::{self, Credentials, Stamp};
use crate::{output, prefs, preview};

/// Keychain service and item names in it.
const KEYCHAIN_SERVICE: &str = "Kadro S3";
pub const ACCESS_KEY: &str = "access-key";
pub const SECRET_KEY: &str = "secret-key";

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    static kSecClass: &'static CFString;
    static kSecClassGenericPassword: &'static CFString;
    static kSecAttrService: &'static CFString;
    static kSecAttrAccount: &'static CFString;
    static kSecValueData: &'static CFString;
    static kSecReturnData: &'static CFString;
    static kSecMatchLimit: &'static CFString;
    static kSecMatchLimitOne: &'static CFString;
    fn SecItemCopyMatching(query: &CFDictionary, result: *mut *const c_void) -> i32;
    fn SecItemAdd(attributes: &CFDictionary, result: *mut *const c_void) -> i32;
    fn SecItemUpdate(query: &CFDictionary, attributes: &CFDictionary) -> i32;
    fn SecItemDelete(query: &CFDictionary) -> i32;
    fn SecRandomCopyBytes(rnd: *const c_void, count: usize, bytes: *mut u8) -> i32;
}

/// errSecItemNotFound.
const ITEM_NOT_FOUND: i32 = -25300;

/// Keychain query for the `account` item and extra fields.
fn keychain_query(account: &str, extra: &[(&CFString, &CFType)]) -> CFRetained<CFDictionary<CFString, CFType>> {
    let service = CFString::from_str(KEYCHAIN_SERVICE);
    let account = CFString::from_str(account);
    // SAFETY: the keys and values are Security system constants, they live for the whole process.
    let mut keys: Vec<&CFString> = unsafe { vec![kSecClass, kSecAttrService, kSecAttrAccount] };
    let mut values: Vec<&CFType> =
        unsafe { vec![kSecClassGenericPassword.as_ref(), service.as_ref(), account.as_ref()] };
    for (key, value) in extra {
        keys.push(key);
        values.push(value);
    }
    CFDictionary::from_slices(&keys, &values)
}

/// Secret from the Keychain. None if it is missing.
pub fn secret(account: &str) -> Option<String> {
    let yes = CFBoolean::new(true);
    // SAFETY: Security system constants live for the whole process.
    let query = unsafe {
        keychain_query(account, &[(kSecReturnData, yes.as_ref()), (kSecMatchLimit, kSecMatchLimitOne.as_ref())])
    };
    let mut result: *const c_void = std::ptr::null();
    // SAFETY: the dictionary lives until the end of the call, the result is a +1 CFData that CFRetained takes over.
    let status = unsafe { SecItemCopyMatching(query.as_opaque(), &raw mut result) };
    if status != 0 || result.is_null() {
        return None;
    }
    let data: CFRetained<CFData> = unsafe { CFRetained::from_raw(std::ptr::NonNull::new(result.cast_mut().cast())?) };
    String::from_utf8(data.to_vec()).ok()
}

/// Writes a secret to the Keychain. An empty value deletes the item. The error is the Keychain status code.
pub fn set_secret(account: &str, value: &str) -> Result<(), i32> {
    let query = keychain_query(account, &[]);
    if value.is_empty() {
        // SAFETY: the dictionary lives until the end of the call.
        let status = unsafe { SecItemDelete(query.as_opaque()) };
        return if status == 0 || status == ITEM_NOT_FOUND { Ok(()) } else { Err(status) };
    }
    let data = CFData::from_bytes(value.as_bytes());
    // SAFETY: the Security system constant lives for the whole process.
    let (key, value): (&CFString, &CFType) = unsafe { (kSecValueData, data.as_ref()) };
    let changes = CFDictionary::<CFString, CFType>::from_slices(&[key], &[value]);
    // Updated in place, so a failed write keeps the old key instead of leaving none.
    // SAFETY: both dictionaries live until the end of the call.
    let mut status = unsafe { SecItemUpdate(query.as_opaque(), changes.as_opaque()) };
    if status == ITEM_NOT_FOUND {
        // SAFETY: the dictionary lives until the end of the call.
        status = unsafe { SecItemAdd(keychain_query(account, &[(key, value)]).as_opaque(), std::ptr::null_mut()) };
    }
    if status != 0 {
        eprintln!("kadro: keychain refused to store the {account} ({status})");
        return Err(status);
    }
    Ok(())
}

/// Random hex with 96 bits of entropy, for object names nobody can guess.
fn random_suffix() -> Result<String, String> {
    let mut bytes = [0u8; 12];
    // SAFETY: the buffer is 12 writable bytes, NULL means the default generator.
    let status = unsafe { SecRandomCopyBytes(std::ptr::null(), bytes.len(), bytes.as_mut_ptr()) };
    if status == 0 { Ok(sigv4::hex(&bytes)) } else { Err(format!("no random bytes for the file name ({status})")) }
}

/// Storage settings.
pub struct Storage {
    /// Empty means AWS, otherwise the address of compatible storage.
    pub endpoint: String,
    pub region: String,
    pub bucket: String,
    pub prefix: String,
    pub public_url: String,
    pub access_key: String,
    pub secret_key: String,
    pub links: LinkKind,
    pub expiry: LinkExpiry,
}

impl Storage {
    pub fn load() -> Self {
        let text = |key| settings::string(key).map(|s| s.trim().to_string()).unwrap_or_default();
        let region = text(settings::UPLOAD_REGION);
        Storage {
            endpoint: text(settings::UPLOAD_ENDPOINT).trim_end_matches('/').to_string(),
            region: if region.is_empty() { "us-east-1".into() } else { region },
            bucket: text(settings::UPLOAD_BUCKET),
            prefix: text(settings::UPLOAD_PREFIX).trim_matches('/').to_string(),
            public_url: text(settings::UPLOAD_PUBLIC_URL).trim_end_matches('/').to_string(),
            access_key: secret(ACCESS_KEY).unwrap_or_default(),
            secret_key: secret(SECRET_KEY).unwrap_or_default(),
            links: settings::choice().unwrap_or(LinkKind::Presigned),
            expiry: settings::choice().unwrap_or(LinkExpiry::Day),
        }
    }

    pub fn is_configured(&self) -> bool {
        !self.bucket.is_empty() && !self.access_key.is_empty() && !self.secret_key.is_empty()
    }

    /// What is wrong with the settings, if requests would go somewhere other than the storage
    /// or in plain text. None if they are fine.
    pub fn problem(&self) -> Option<String> {
        // The bucket and region go into the host name, "/", "@" or "#" there would send the request to another host.
        let plain = |text: &str, extra: &str| {
            !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c))
        };
        if !plain(&self.bucket, "-._") {
            return Some("The bucket name can only have letters, digits, dots, dashes and underscores.".into());
        }
        if !plain(&self.region, "-") {
            return Some("The region can only have letters, digits and dashes.".into());
        }
        if self.endpoint.is_empty() {
            return None;
        }
        let (scheme, rest) = self.endpoint.split_once("://").unwrap_or(("https", &self.endpoint));
        if rest.contains(['@', '?', '#']) {
            return Some("The endpoint must be a plain address like https://s3.example.com.".into());
        }
        let host = rest.split('/').next().unwrap_or(rest);
        let local = ["localhost", "127.0.0.1", "[::1]"]
            .iter()
            .any(|name| host == *name || host.strip_prefix(name).is_some_and(|port| port.starts_with(':')));
        match scheme {
            "https" => None,
            "http" if local => None,
            _ => Some("The endpoint must start with https://, plain http:// is allowed only for this Mac.".into()),
        }
    }

    fn credentials(&self) -> Credentials<'_> {
        Credentials { access_key: &self.access_key, secret_key: &self.secret_key, region: &self.region }
    }

    /// Scheme, host and object path. AWS puts the bucket in the host name, others in the path.
    pub fn locate(&self, key: &str) -> (String, String, String) {
        if self.endpoint.is_empty() {
            let host = format!("{}.s3.{}.amazonaws.com", self.bucket, self.region);
            return ("https".into(), host, format!("/{key}"));
        }
        let (scheme, rest) = self.endpoint.split_once("://").unwrap_or(("https", &self.endpoint));
        let host = rest.split('/').next().unwrap_or(rest).to_string();
        (scheme.to_string(), host, format!("/{}/{key}", self.bucket))
    }

    fn key(&self, name: &str) -> String {
        if self.prefix.is_empty() { name.to_string() } else { format!("{}/{name}", self.prefix) }
    }

    /// Signed request to the object. The body is empty for DELETE.
    fn request(
        &self,
        method: &str,
        key: &str,
        body: &[u8],
        content_type: Option<&str>,
        now: u64,
    ) -> (String, Vec<(String, String)>) {
        let (scheme, host, path) = self.locate(key);
        let stamp = sigv4::stamp(now);
        let payload = sigv4::hex(&sigv4::sha256(body));
        let mut headers =
            vec![("host", host.clone()), ("x-amz-content-sha256", payload.clone()), ("x-amz-date", stamp.clone())];
        if let Some(content_type) = content_type {
            headers.push(("content-type", content_type.to_string()));
        }
        let authorization = sigv4::authorization(&self.credentials(), Stamp(&stamp), method, &path, &headers, &payload);
        let mut headers: Vec<(String, String)> =
            headers.into_iter().filter(|(k, _)| *k != "host").map(|(k, v)| (k.to_string(), v)).collect();
        headers.push(("authorization".into(), authorization));
        (format!("{scheme}://{host}{}", sigv4::encode(&path, true)), headers)
    }

    /// Link to the object per settings. None means no link is needed.
    pub fn link(&self, key: &str, now: u64) -> Option<String> {
        let (scheme, host, path) = self.locate(key);
        match self.links {
            LinkKind::None => None,
            LinkKind::Public if !self.public_url.is_empty() => {
                Some(format!("{}/{}", self.public_url, sigv4::encode(key, true)))
            }
            LinkKind::Public => Some(format!("{scheme}://{host}{}", sigv4::encode(&path, true))),
            LinkKind::Presigned => {
                let stamp = sigv4::stamp(now);
                let query =
                    sigv4::presigned_query(&self.credentials(), Stamp(&stamp), &host, &path, self.expiry.seconds());
                Some(format!("{scheme}://{host}{}?{query}", sigv4::encode(&path, true)))
            }
        }
    }

    /// Uploads an object. Returns the link per settings.
    pub fn put(&self, name: &str, body: &[u8], content_type: &str) -> Result<Option<String>, String> {
        if let Some(problem) = self.problem() {
            return Err(problem);
        }
        let key = self.key(name);
        let now = unix_now();
        let (url, headers) = self.request("PUT", &key, body, Some(content_type), now);
        send("PUT", &url, &headers, body)?;
        Ok(self.link(&key, now))
    }

    /// Uploads and deletes a small file, which checks the keys, bucket and write permission.
    pub fn test(&self) -> Result<(), String> {
        if let Some(problem) = self.problem() {
            return Err(problem);
        }
        let key = self.key(".kadro-test");
        let (url, headers) = self.request("PUT", &key, b"Kadro", Some("text/plain"), unix_now());
        send("PUT", &url, &headers, b"Kadro")?;
        let (url, headers) = self.request("DELETE", &key, b"", None, unix_now());
        send("DELETE", &url, &headers, b"").map(drop)
    }
}

fn unix_now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Sends a request and waits for the response. Returns the response body. Called only off the main thread.
pub fn send(method: &str, url: &str, headers: &[(String, String)], body: &[u8]) -> Result<Vec<u8>, String> {
    let url = NSURL::URLWithString(&NSString::from_str(url)).ok_or("the address is not a valid URL")?;
    let request = NSMutableURLRequest::requestWithURL(&url);
    request.setHTTPMethod(&NSString::from_str(method));
    for (name, value) in headers {
        request.setValue_forHTTPHeaderField(Some(&NSString::from_str(value)), &NSString::from_str(name));
    }
    if !body.is_empty() {
        request.setHTTPBody(Some(&NSData::with_bytes(body)));
    }
    let (sender, receiver) = mpsc::channel();
    let handler = RcBlock::new(move |data: *mut NSData, response: *mut NSURLResponse, error: *mut NSError| {
        let result = match unsafe { error.as_ref() } {
            Some(error) => Err(error.localizedDescription().to_string()),
            None => {
                let status = unsafe { response.as_ref() }
                    .and_then(|r| r.downcast_ref::<NSHTTPURLResponse>())
                    .map_or(0, NSHTTPURLResponse::statusCode);
                let body = unsafe { data.as_ref() }.map(NSData::to_vec).unwrap_or_default();
                if (200..300).contains(&status) { Ok(body) } else { Err(error_message(status, &body)) }
            }
        };
        let _ = sender.send(result);
    });
    let task = unsafe { NSURLSession::sharedSession().dataTaskWithRequest_completionHandler(&request, &handler) };
    task.resume();
    receiver.recv().unwrap_or_else(|_| Err("the request was dropped".into()))
}

/// Error text from an S3 XML response: `<Code>` and `<Message>`, if present.
pub fn error_message(status: isize, body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let tag = |name: &str| {
        let open = format!("<{name}>");
        let start = text.find(&open)? + open.len();
        let end = text[start..].find(&format!("</{name}>"))? + start;
        Some(text[start..end].to_string())
    };
    match (tag("Code"), tag("Message")) {
        (Some(code), Some(message)) => format!("{message} ({code}, HTTP {status})"),
        (Some(code), None) => format!("{code} (HTTP {status})"),
        _ => format!("The storage answered HTTP {status}"),
    }
}

fn mime(extension: &str) -> &'static str {
    match extension {
        "jpg" => "image/jpeg",
        "heic" => "image/heic",
        _ => "image/png",
    }
}

/// Uploads the screenshot and puts the link on the clipboard. Without settings offers to open them.
pub fn upload(mtm: MainThreadMarker, image: CFRetained<CGImage>, scale: f64, shot: Option<Shot>) {
    let storage = Storage::load();
    if !storage.is_configured() {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(ns_string!("Upload isn't set up yet"));
        alert.setInformativeText(ns_string!("Add your storage bucket and keys in Settings → Upload."));
        alert.addButtonWithTitle(ns_string!("Open Settings"));
        alert.addButtonWithTitle(ns_string!("Cancel"));
        NSApplication::sharedApplication(mtm).activate();
        if alert.runModal() == NSAlertFirstButtonReturn {
            prefs::show(mtm);
        }
        return;
    }
    popup(mtm, "Uploading…");
    output::in_background(move || {
        let result = output::encoded(&image, scale, shot.as_ref(), None).map_err(|err| err.to_string()).and_then(
            |(bytes, name, format)| {
                let extension = format.extension();
                // Names are the capture time to the second, so in a public bucket they could be guessed.
                let name = format!("{name} {}.{extension}", random_suffix()?);
                storage.put(&name, &bytes, mime(extension))
            },
        );
        DispatchQueue::main().exec_async(move || {
            let Some(mtm) = MainThreadMarker::new() else { return };
            match result {
                Ok(Some(link)) => {
                    copy(&link);
                    popup(mtm, "Link copied");
                }
                Ok(None) => popup(mtm, "Uploaded"),
                Err(message) => {
                    let alert = NSAlert::new(mtm);
                    alert.setMessageText(ns_string!("Couldn't upload the screenshot"));
                    alert.setInformativeText(&NSString::from_str(&message));
                    NSApplication::sharedApplication(mtm).activate();
                    alert.runModal();
                }
            }
        });
    });
}

/// Uploads the last screenshot. If there are no screenshots yet, only beeps.
pub fn upload_last(mtm: MainThreadMarker) {
    match crate::pin::load_last() {
        Some((image, scale)) => upload(mtm, image, scale, crate::pin::last_shot()),
        None => objc2_app_kit::NSBeep(),
    }
}

fn popup(mtm: MainThreadMarker, text: &str) {
    if let Some(screen) = NSScreen::mainScreen(mtm) {
        preview::popup_on(mtm, text, &screen);
    }
}

fn copy(text: &str) {
    let pasteboard = NSPasteboard::generalPasteboard();
    pasteboard.clearContents();
    // SAFETY: the system pasteboard type constant lives for the whole process.
    pasteboard.setString_forType(&NSString::from_str(text), unsafe { NSPasteboardTypeString });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storage(endpoint: &str, links: LinkKind, public_url: &str) -> Storage {
        Storage {
            endpoint: endpoint.into(),
            region: "auto".into(),
            bucket: "shots".into(),
            prefix: "kadro".into(),
            public_url: public_url.into(),
            access_key: "AKIAIOSFODNN7EXAMPLE".into(),
            secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
            links,
            expiry: LinkExpiry::Week,
        }
    }

    #[test]
    fn aws_puts_the_bucket_in_the_host_and_others_in_the_path() {
        let aws = Storage { region: "eu-west-1".into(), ..storage("", LinkKind::None, "") };
        assert_eq!(
            aws.locate("kadro/a.png"),
            ("https".into(), "shots.s3.eu-west-1.amazonaws.com".into(), "/kadro/a.png".into())
        );
        let r2 = storage("https://acc.r2.cloudflarestorage.com/", LinkKind::None, "");
        let r2 = Storage { endpoint: r2.endpoint.trim_end_matches('/').into(), ..r2 };
        assert_eq!(r2.locate("a.png"), ("https".into(), "acc.r2.cloudflarestorage.com".into(), "/shots/a.png".into()));
        let local = storage("http://127.0.0.1:9000", LinkKind::None, "");
        assert_eq!(local.locate("a.png").1, "127.0.0.1:9000");
    }

    #[test]
    fn links_follow_the_setting() {
        let now = 1_369_353_600;
        let key = "kadro/Shot 1.png";
        assert_eq!(storage("https://s3.example.com", LinkKind::None, "").link(key, now), None);
        assert_eq!(
            storage("https://s3.example.com", LinkKind::Public, "https://img.example.com").link(key, now).unwrap(),
            "https://img.example.com/kadro/Shot%201.png"
        );
        assert_eq!(
            storage("https://s3.example.com", LinkKind::Public, "").link(key, now).unwrap(),
            "https://s3.example.com/shots/kadro/Shot%201.png"
        );
        let presigned = storage("https://s3.example.com", LinkKind::Presigned, "").link(key, now).unwrap();
        assert!(
            presigned.starts_with("https://s3.example.com/shots/kadro/Shot%201.png?X-Amz-Algorithm=AWS4-HMAC-SHA256")
        );
        assert!(presigned.contains("&X-Amz-Expires=604800&"));
    }

    #[test]
    fn upload_request_is_signed_with_the_body_hash() {
        let s = storage("https://s3.example.com", LinkKind::None, "");
        let (url, headers) = s.request("PUT", "kadro/a.png", b"png", Some("image/png"), 1_369_353_600);
        assert_eq!(url, "https://s3.example.com/shots/kadro/a.png");
        let header = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str());
        assert_eq!(header("x-amz-date"), Some("20130524T000000Z"));
        assert_eq!(header("x-amz-content-sha256"), Some(sigv4::hex(&sigv4::sha256(b"png"))).as_deref());
        let authorization = header("authorization").unwrap();
        assert!(
            authorization
                .starts_with("AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20130524/auto/s3/aws4_request,")
        );
        assert!(authorization.contains("SignedHeaders=content-type;host;x-amz-content-sha256;x-amz-date,"));
    }

    #[test]
    fn settings_that_would_redirect_or_expose_requests_are_refused() {
        assert_eq!(storage("", LinkKind::None, "").problem(), None);
        assert_eq!(storage("https://acc.r2.cloudflarestorage.com", LinkKind::None, "").problem(), None);
        assert_eq!(storage("http://127.0.0.1:9000", LinkKind::None, "").problem(), None);
        assert_eq!(storage("http://localhost", LinkKind::None, "").problem(), None);
        // cleartext to another machine
        assert!(storage("http://s3.example.com", LinkKind::None, "").problem().is_some());
        assert!(storage("http://localhost.evil.com", LinkKind::None, "").problem().is_some());
        assert!(storage("https://user@evil.com", LinkKind::None, "").problem().is_some());
        // the bucket would become part of the host
        let aws = |bucket: &str| Storage { bucket: bucket.into(), ..storage("", LinkKind::None, "") };
        assert!(aws("evil.com/x").problem().is_some());
        assert!(aws("evil.com#").problem().is_some());
        assert!(aws("").problem().is_some());
        let region = Storage { region: "x.evil.com/".into(), ..storage("", LinkKind::None, "") };
        assert!(region.problem().is_some());
    }

    #[test]
    fn upload_names_get_unguessable_suffixes() {
        let (a, b) = (random_suffix().unwrap(), random_suffix().unwrap());
        assert_eq!(a.len(), 24);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn storage_errors_read_the_s3_message() {
        let body = b"<?xml version=\"1.0\"?><Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>";
        assert_eq!(error_message(403, body), "Access Denied (AccessDenied, HTTP 403)");
        assert_eq!(error_message(502, b""), "The storage answered HTTP 502");
    }
}
