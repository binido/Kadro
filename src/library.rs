//! Библиотека снимков: индекс сохранённых файлов с распознанным текстом для поиска.
//!
//! Индекс - текстовый файл в Application Support, по строке на снимок: путь, время изменения,
//! распознан ли текст и сам текст. Текст распознаётся в фоне по одному снимку за раз, так что
//! поиск по нему работает и для снимков, сделанных до включения библиотеки.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use dispatch2::{DispatchQoS, DispatchQueue, GlobalQueueIdentifier};

use crate::recognize;
use crate::settings::Settings;

/// Расширения картинок, которые библиотека берёт из папки снимков.
const EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "heic", "gif"];
/// Сколько снимков распознаёт один запуск отдельного процесса. После каждого индекс записывается на диск.
const BATCH: usize = 20;

static INDEX: Mutex<Option<Index>> = Mutex::new(None);
static RECOGNIZING: AtomicBool = AtomicBool::new(false);

/// Снимок в библиотеке.
#[derive(Clone, PartialEq, Debug)]
pub struct Entry {
    pub path: PathBuf,
    /// Время изменения файла, секунды Unix.
    pub modified: u64,
    /// Текст уже распознан, даже если его не нашлось.
    pub recognized: bool,
    pub text: String,
}

impl Entry {
    pub fn name(&self) -> String {
        self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default()
    }

    /// Подходит ли снимок под запрос: все слова запроса есть в имени, дате или тексте.
    pub fn matches(&self, query: &str, date: &str) -> bool {
        let haystack = format!("{} {date} {}", self.name(), self.text).to_lowercase();
        query.to_lowercase().split_whitespace().all(|word| haystack.contains(word))
    }
}

/// Снимки от новых к старым.
#[derive(Clone, Default, PartialEq, Debug)]
pub struct Index {
    pub entries: Vec<Entry>,
}

impl Index {
    pub fn parse(text: &str) -> Self {
        let entries = text
            .lines()
            .filter_map(|line| {
                let mut fields = line.splitn(4, '\t');
                let path = PathBuf::from(fields.next()?);
                let modified = fields.next()?.parse().ok()?;
                let recognized = fields.next()? == "1";
                let text = fields.next().unwrap_or_default().to_string();
                Some(Entry { path, modified, recognized, text })
            })
            .collect();
        let mut index = Index { entries };
        index.sort();
        index
    }

    pub fn serialize(&self) -> String {
        self.entries
            .iter()
            .map(|e| {
                // Табуляция и переносы - разделители файла, в тексте они становятся пробелами.
                let text: String =
                    e.text.chars().map(|c| if c == '\t' || c == '\n' || c == '\r' { ' ' } else { c }).collect();
                format!("{}\t{}\t{}\t{text}\n", e.path.display(), e.modified, u8::from(e.recognized))
            })
            .collect()
    }

    fn sort(&mut self) {
        self.entries.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| a.path.cmp(&b.path)));
    }

    /// Добавляет снимки, которых ещё нет, и отбрасывает самые старые сверх `limit`.
    pub fn add(&mut self, files: impl IntoIterator<Item = (PathBuf, u64)>, limit: usize) {
        for (path, modified) in files {
            match self.entries.iter_mut().find(|e| e.path == path) {
                // Файл перезаписали - текст надо распознать заново.
                Some(entry) if entry.modified != modified => {
                    *entry = Entry { path, modified, recognized: false, text: String::new() };
                }
                Some(_) => {}
                None => self.entries.push(Entry { path, modified, recognized: false, text: String::new() }),
            }
        }
        self.sort();
        self.entries.truncate(limit);
    }

    pub fn remove(&mut self, path: &Path) {
        self.entries.retain(|e| e.path != path);
    }

    /// Убирает снимки, файлов которых больше нет.
    pub fn prune(&mut self) {
        self.entries.retain(|e| e.path.exists());
    }
}

fn index_path() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join("Library/Application Support/Kadro/library.tsv")
}

/// Работает с индексом, загружая его с диска при первом обращении.
fn with_index<R>(f: impl FnOnce(&mut Index) -> R) -> R {
    let mut guard = INDEX.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let index = guard.get_or_insert_with(|| {
        std::fs::read_to_string(index_path()).map(|text| Index::parse(&text)).unwrap_or_default()
    });
    f(index)
}

fn store(index: &Index) {
    let path = index_path();
    let written = path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| {
        // Сначала во временный файл: оборванная запись не испортит индекс.
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, index.serialize()).and_then(|()| std::fs::rename(&temporary, &path))
    });
    if let Err(err) = written {
        eprintln!("kadro: failed to write the library index: {err}");
    }
}

fn modified(path: &Path) -> Option<u64> {
    let time = std::fs::metadata(path).ok()?.modified().ok()?;
    time.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

/// Снимки библиотеки от новых к старым.
pub fn entries() -> Vec<Entry> {
    with_index(|index| index.entries.clone())
}

/// Был ли индекс уже создан. До первого открытия библиотеки его нет.
pub fn exists() -> bool {
    index_path().exists()
}

/// Добавляет только что сохранённый снимок и распознаёт его текст.
pub fn add(path: &Path) {
    let settings = Settings::load();
    if !settings.library {
        return;
    }
    let Some(time) = modified(path) else { return };
    with_index(|index| {
        index.add([(path.to_path_buf(), time)], settings.library_limit.items());
        store(index);
    });
    recognize_pending();
}

/// Забирает картинки из папки в индекс и убирает пропавшие файлы.
pub fn import(directory: &Path) {
    let files: Vec<(PathBuf, u64)> = std::fs::read_dir(directory)
        .map(|dir| {
            dir.filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| EXTENSIONS.contains(&e.to_lowercase().as_str()))
                })
                .filter_map(|path| modified(&path).map(|time| (path, time)))
                .collect()
        })
        .unwrap_or_default();
    let limit = Settings::load().library_limit.items();
    with_index(|index| {
        index.prune();
        index.add(files, limit);
        store(index);
    });
    recognize_pending();
}

/// Убирает из индекса снимки, файлов которых больше нет.
pub fn prune() {
    with_index(|index| {
        index.prune();
        store(index);
    });
}

/// Убирает снимок из индекса, например после удаления файла.
pub fn forget(path: &Path) {
    with_index(|index| {
        index.remove(path);
        store(index);
    });
}

/// Распознаёт текст снимков, у которых его ещё нет. Работает один фоновый проход за раз
/// с низким приоритетом, чтобы не мешать захвату.
pub fn recognize_pending() {
    if RECOGNIZING.swap(true, Ordering::SeqCst) {
        return;
    }
    DispatchQueue::global_queue(GlobalQueueIdentifier::QualityOfService(DispatchQoS::Utility)).exec_async(|| {
        let language = Settings::load().recognition_language;
        loop {
            let batch: Vec<PathBuf> = with_index(|index| {
                index.entries.iter().filter(|e| !e.recognized).take(BATCH).map(|e| e.path.clone()).collect()
            });
            if batch.is_empty() {
                break;
            }
            let found = recognize::recognize_files(&batch, language.as_deref());
            with_index(|index| {
                for (path, found) in batch.iter().zip(found) {
                    if let Some(entry) = index.entries.iter_mut().find(|e| &e.path == path) {
                        entry.recognized = true;
                        entry.text = found.lines.join(" ");
                    }
                }
                store(index);
            });
        }
        RECOGNIZING.store(false, Ordering::SeqCst);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, modified: u64, text: &str) -> Entry {
        Entry { path: PathBuf::from(path), modified, recognized: !text.is_empty(), text: text.into() }
    }

    #[test]
    fn index_round_trips_and_keeps_newest_first() {
        let index =
            Index { entries: vec![entry("/a/old.png", 10, ""), entry("/a/new.png", 20, "hello\tworld\nagain")] };
        let parsed = Index::parse(&index.serialize());
        assert_eq!(parsed.entries[0].path, PathBuf::from("/a/new.png"));
        assert_eq!(parsed.entries[0].text, "hello world again");
        assert!(parsed.entries[0].recognized);
        assert_eq!(parsed.entries[1], entry("/a/old.png", 10, ""));
        // битые строки пропускаются
        assert_eq!(Index::parse("garbage\n/x.png\tnot-a-number\t0\t\n").entries, []);
    }

    #[test]
    fn adding_dedupes_rerecognizes_changed_files_and_caps_the_count() {
        let mut index = Index { entries: vec![entry("/a/1.png", 10, "first")] };
        index.add(
            [(PathBuf::from("/a/1.png"), 10), (PathBuf::from("/a/2.png"), 30), (PathBuf::from("/a/3.png"), 20)],
            10,
        );
        assert_eq!(index.entries.len(), 3);
        assert_eq!(index.entries[2].text, "first");
        // файл перезаписан: текст распознаётся заново
        index.add([(PathBuf::from("/a/1.png"), 40)], 10);
        assert_eq!((index.entries[0].modified, index.entries[0].recognized), (40, false));
        // лимит отбрасывает самые старые
        index.add([], 2);
        let names: Vec<_> = index.entries.iter().map(Entry::name).collect();
        assert_eq!(names, ["1", "2"]);
    }

    #[test]
    fn search_needs_every_word_in_name_date_or_text() {
        let shot = entry("/a/Kadro 2026-11-07 at 12.00.png", 0, "Invoice total 42 EUR");
        assert!(shot.matches("invoice eur", "2026-11-07"));
        assert!(shot.matches("2026-11", "2026-11-07"));
        assert!(shot.matches("", "2026-11-07"));
        assert!(!shot.matches("invoice usd", "2026-11-07"));
    }
}
