//! Bounded, immutable capture journal. All filesystem work runs off the proxy path.
use super::{
    TraceEvent,
    memory::{MemoryBudget, Reservation},
};
use crate::config::JournalConfig;
use anyhow::{Context, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime},
};
use tokio::sync::Notify;
use uuid::Uuid;

const MAGIC: &[u8; 8] = b"LLMTJ001";
const HEADER_BYTES: u64 = 72;
const MAX_METADATA_BYTES: u64 = 16 * 1024 * 1024;
const MAX_SCAN_RECORDS: usize = 1_000_000;

#[derive(Serialize, Deserialize)]
struct Manifest {
    version: u32,
    id: Uuid,
    database: String,
}

#[derive(Clone)]
struct Entry {
    size: u64,
    memory: usize,
    created: SystemTime,
    due: Instant,
    recovered: bool,
}

#[derive(Default)]
struct Catalog {
    entries: BTreeMap<Uuid, Entry>,
    ready: VecDeque<Uuid>,
    bytes: u64,
    quarantined: u64,
    quarantine_bytes: u64,
    quarantine_ids: HashSet<Uuid>,
}

pub(super) struct Journal {
    pub id: Uuid,
    pub config: JournalConfig,
    _lock: File,
    root: PathBuf,
    catalog: Mutex<Catalog>,
    pub notify: Notify,
}

pub(super) struct Claim {
    pub id: Uuid,
    pub recovered: bool,
    pub _reservation: Reservation,
    journal: Arc<Journal>,
}

impl Drop for Claim {
    fn drop(&mut self) {
        self.journal.retry(self.id);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct JournalSnapshot {
    pub enabled: bool,
    pub pending_records: u64,
    pub pending_bytes: u64,
    pub max_bytes: u64,
    pub max_records: u64,
    pub quarantined_records: u64,
    pub quarantined_bytes: u64,
    pub oldest_pending_age_secs: u64,
    pub blocked_records: u64,
}

#[derive(Debug, thiserror::Error)]
#[error("capture journal capacity exhausted")]
pub(super) struct Full;

impl Journal {
    pub fn open(config: &JournalConfig, database_url: &str) -> anyhow::Result<Arc<Self>> {
        fs::create_dir_all(&config.directory).context("create capture journal directory")?;
        ensure!(
            !fs::symlink_metadata(&config.directory)?
                .file_type()
                .is_symlink(),
            "journal directory must not be a symlink"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&config.directory, fs::Permissions::from_mode(0o700))?;
        }
        let root = fs::canonicalize(&config.directory)?;
        let lock_path = root.join("lock");
        reject_symlink(&lock_path)?;
        let lock = private_options()
            .create(true)
            .truncate(false)
            .open(&lock_path)?;
        lock.try_lock()
            .context("journal directory is already in use (use one directory per instance)")?;
        // Use the same URL interpretation as the database client, including
        // host/port/dbname query overrides. Never persist connection credentials.
        let options: sqlx::postgres::PgConnectOptions = database_url
            .parse()
            .context("invalid journal database target")?;
        let target = serde_json::to_vec(&(
            options.get_host(),
            options.get_port(),
            options.get_socket(),
            options.get_database().unwrap_or(options.get_username()),
        ))?;
        let database = hex::encode(Sha256::digest(target));
        let manifest_path = root.join("manifest.json");
        reject_symlink(&manifest_path)?;
        let manifest = if manifest_path.exists() {
            ensure!(
                fs::metadata(&manifest_path)?.len() <= 4096,
                "invalid journal manifest"
            );
            let manifest: Manifest = serde_json::from_reader(File::open(&manifest_path)?)?;
            ensure!(
                manifest.version == 1 && manifest.database == database,
                "journal belongs to another database or format; preserve it and use a different directory"
            );
            manifest
        } else {
            // Never adopt existing captures after losing their receipt namespace.
            ensure!(
                fs::read_dir(&root)?.all(|entry| entry
                    .is_ok_and(|e| e.file_name() == "lock" || e.file_name() == "manifest.partial")),
                "journal manifest missing from a nonempty directory"
            );
            let manifest = Manifest {
                version: 1,
                id: Uuid::new_v4(),
                database,
            };
            let tmp = root.join("manifest.partial");
            if tmp.exists() {
                reject_symlink(&tmp)?;
                fs::remove_file(&tmp)?;
            }
            let mut file = private_options().create_new(true).open(&tmp)?;
            serde_json::to_writer(&mut file, &manifest)?;
            file.sync_all()?;
            fs::rename(&tmp, &manifest_path)?;
            sync_directory(&root)?;
            manifest
        };
        let mut catalog = Catalog::default();
        for file in fs::read_dir(&root)? {
            let file = file?;
            let name = file.file_name();
            let name = name.to_string_lossy();
            if matches!(name.as_ref(), "lock" | "manifest.json" | "manifest.partial") {
                continue;
            }
            ensure!(
                file.file_type()?.is_file(),
                "unexpected non-file in capture journal"
            );
            ensure!(
                catalog.entries.len() + (catalog.quarantined as usize) < MAX_SCAN_RECORDS,
                "too many journal files to recover safely"
            );
            let path = file.path();
            let size = file.metadata()?.len();
            let parsed = name
                .strip_suffix(".trace")
                .and_then(|s| Uuid::parse_str(s).ok());
            if let Some(id) = parsed
                && let Ok((_, _, _, memory, _)) = read_header(&mut File::open(&path)?, size)
            {
                catalog.entries.insert(
                    id,
                    Entry {
                        size,
                        memory,
                        created: file.metadata()?.modified()?,
                        due: Instant::now(),
                        recovered: true,
                    },
                );
                catalog.bytes = catalog.bytes.saturating_add(size);
                continue;
            }
            // Interrupted writes and malformed records remain for investigation.
            if !name.ends_with(".quarantine") {
                fs::rename(
                    &path,
                    root.join(format!("{name}.{}.quarantine", Uuid::new_v4())),
                )?;
            }
            if let Some(id) = name
                .split('.')
                .next()
                .and_then(|id| Uuid::parse_str(id).ok())
            {
                catalog.quarantine_ids.insert(id);
            }
            catalog.quarantined += 1;
            catalog.quarantine_bytes = catalog.quarantine_bytes.saturating_add(size);
        }
        let mut ordered: Vec<_> = catalog
            .entries
            .iter()
            .map(|(id, e)| (e.created, *id))
            .collect();
        ordered.sort_unstable();
        catalog.ready = ordered.into_iter().map(|(_, id)| id).collect();
        // Make newly created directory entries durable too, including parents.
        for parent in root.ancestors().skip(1) {
            sync_directory(parent)?;
        }
        // Flush observed deletions/renames before orphan receipt cleanup is allowed.
        sync_directory(&root)?;
        Ok(Arc::new(Self {
            id: manifest.id,
            config: config.clone(),
            _lock: lock,
            root,
            catalog: Mutex::new(catalog),
            notify: Notify::new(),
        }))
    }

    pub fn append(&self, event: &TraceEvent) -> anyhow::Result<()> {
        let metadata = serde_json::to_vec(event)?;
        ensure!(
            metadata.len() as u64 <= MAX_METADATA_BYTES,
            "journal metadata exceeds 16 MiB"
        );
        let request = event.request_body.len() as u64;
        let response = event.response_body.len() as u64;
        let size = HEADER_BYTES + metadata.len() as u64 + request + response;
        // Conservative replay allocation reservation, including JSON container overhead.
        let memory = super::memory::event_bytes(event).max(
            event
                .request_body
                .len()
                .saturating_add(event.response_body.len())
                .saturating_add(metadata.len().saturating_mul(8))
                .saturating_add(65536),
        );
        {
            let mut catalog = self.catalog.lock().unwrap();
            ensure!(
                !catalog.entries.contains_key(&event.id),
                "duplicate capture journal id"
            );
            if catalog
                .bytes
                .saturating_add(catalog.quarantine_bytes)
                .saturating_add(size)
                > self.config.max_bytes
                || catalog
                    .entries
                    .len()
                    .saturating_add(catalog.quarantined as usize)
                    >= self.config.max_records
            {
                return Err(Full.into());
            }
            catalog.bytes += size;
            catalog.entries.insert(
                event.id,
                Entry {
                    size,
                    memory,
                    created: SystemTime::now(),
                    due: Instant::now(),
                    recovered: false,
                },
            );
        }
        let temporary = self.root.join(format!("{}.partial", event.id));
        let destination = self.path(event.id);
        let result = (|| -> anyhow::Result<()> {
            let prefix = header_prefix(metadata.len() as u64, request, response, memory as u64);
            let mut digest = Sha256::new();
            digest.update(prefix);
            digest.update(&metadata);
            digest.update(&event.request_body);
            digest.update(&event.response_body);
            let mut file = private_options().create_new(true).open(&temporary)?;
            file.write_all(&prefix)?;
            file.write_all(&digest.finalize())?;
            file.write_all(&metadata)?;
            file.write_all(&event.request_body)?;
            file.write_all(&event.response_body)?;
            file.sync_all()?;
            ensure!(!destination.exists(), "journal destination already exists");
            fs::rename(&temporary, &destination)?;
            sync_directory(&self.root)?;
            Ok(())
        })();
        if let Err(error) = result {
            // Preserve ambiguous writes. Never silently fall back to a volatile record.
            self.quarantine(event.id)?;
            return Err(error);
        }
        self.catalog.lock().unwrap().ready.push_back(event.id);
        self.notify.notify_one();
        Ok(())
    }

    pub fn claim(self: &Arc<Self>, memory: &Arc<MemoryBudget>) -> Option<Claim> {
        let mut catalog = self.catalog.lock().unwrap();
        for _ in 0..catalog.ready.len() {
            let id = catalog.ready.pop_front()?;
            let entry = catalog.entries.get(&id)?;
            if entry.due <= Instant::now()
                && let Some(reservation) = memory.reserve(entry.memory)
            {
                return Some(Claim {
                    id,
                    recovered: entry.recovered,
                    _reservation: reservation,
                    journal: self.clone(),
                });
            }
            catalog.ready.push_back(id);
        }
        None
    }

    pub fn retry(&self, id: Uuid) {
        let mut catalog = self.catalog.lock().unwrap();
        if let Some(entry) = catalog.entries.get_mut(&id) {
            entry.due = Instant::now() + Duration::from_secs(self.config.retry_interval_secs);
            catalog.ready.push_back(id);
        }
    }

    pub fn read(&self, id: Uuid) -> anyhow::Result<TraceEvent> {
        let path = self.path(id);
        reject_symlink(&path)?;
        let mut file = File::open(&path)?;
        let size = file.metadata()?.len();
        let (metadata, request, response, memory, checksum) = read_header(&mut file, size)?;
        let mut meta = vec![0; metadata];
        let mut request_body = vec![0; request];
        let mut response_body = vec![0; response];
        file.read_exact(&mut meta)?;
        file.read_exact(&mut request_body)?;
        file.read_exact(&mut response_body)?;
        let mut digest = Sha256::new();
        digest.update(header_prefix(
            metadata as u64,
            request as u64,
            response as u64,
            memory as u64,
        ));
        digest.update(&meta);
        digest.update(&request_body);
        digest.update(&response_body);
        ensure!(
            digest.finalize().as_slice() == checksum,
            "journal checksum mismatch"
        );
        let mut event: TraceEvent = serde_json::from_slice(&meta)?;
        ensure!(event.id == id, "journal trace id mismatch");
        event.request_body = request_body;
        event.response_body = response_body;
        Ok(event)
    }

    pub fn acknowledge(&self, id: Uuid) -> anyhow::Result<()> {
        match fs::remove_file(self.path(id)) {
            Ok(()) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
        sync_directory(&self.root)?;
        let mut catalog = self.catalog.lock().unwrap();
        if let Some(entry) = catalog.entries.remove(&id) {
            catalog.bytes -= entry.size;
        }
        Ok(())
    }

    pub fn quarantine(&self, id: Uuid) -> anyhow::Result<()> {
        let destination = self
            .root
            .join(format!("{id}.{}.quarantine", Uuid::new_v4()));
        let source = if self.path(id).exists() {
            self.path(id)
        } else {
            self.root.join(format!("{id}.partial"))
        };
        let size = if source.exists() {
            let size = fs::metadata(&source)?.len();
            fs::rename(source, &destination)?;
            size
        } else {
            0
        };
        sync_directory(&self.root)?;
        let mut catalog = self.catalog.lock().unwrap();
        if let Some(entry) = catalog.entries.remove(&id) {
            catalog.bytes -= entry.size;
        }
        if destination.exists() {
            catalog.quarantined += 1;
            catalog.quarantine_bytes += size;
            catalog.quarantine_ids.insert(id);
        }
        Ok(())
    }

    pub fn receipt_can_be_removed(&self, id: Uuid) -> bool {
        let catalog = self.catalog.lock().unwrap();
        !catalog.entries.contains_key(&id) && !catalog.quarantine_ids.contains(&id)
    }

    pub fn snapshot(&self, memory_limit: usize) -> JournalSnapshot {
        let catalog = self.catalog.lock().unwrap();
        JournalSnapshot {
            enabled: true,
            pending_records: catalog.entries.len() as u64,
            pending_bytes: catalog.bytes,
            max_bytes: self.config.max_bytes,
            max_records: self.config.max_records as u64,
            quarantined_records: catalog.quarantined,
            quarantined_bytes: catalog.quarantine_bytes,
            oldest_pending_age_secs: catalog
                .entries
                .values()
                .filter_map(|e| e.created.elapsed().ok())
                .max()
                .unwrap_or_default()
                .as_secs(),
            blocked_records: catalog
                .entries
                .values()
                .filter(|e| e.memory > memory_limit)
                .count() as u64,
        }
    }

    fn path(&self, id: Uuid) -> PathBuf {
        self.root.join(format!("{id}.trace"))
    }
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    options.read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}
fn reject_symlink(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(meta) => ensure!(
            meta.is_file() && !meta.file_type().is_symlink(),
            "journal path must be a regular file"
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
fn sync_directory(root: &Path) -> std::io::Result<()> {
    File::open(root)?.sync_all()
}

type Header = (usize, usize, usize, usize, [u8; 32]);
fn read_header(file: &mut File, size: u64) -> anyhow::Result<Header> {
    let mut header = [0; HEADER_BYTES as usize];
    file.read_exact(&mut header)?;
    ensure!(&header[..8] == MAGIC, "unknown journal format");
    let number = |offset| u64::from_le_bytes(header[offset..offset + 8].try_into().unwrap());
    let (meta, request, response, memory) = (number(8), number(16), number(24), number(32));
    ensure!(meta <= MAX_METADATA_BYTES, "journal metadata too large");
    let expected = HEADER_BYTES
        .checked_add(meta)
        .and_then(|n| n.checked_add(request))
        .and_then(|n| n.checked_add(response));
    ensure!(expected == Some(size), "journal record length mismatch");
    let minimum = request
        .checked_add(response)
        .and_then(|n| n.checked_add(meta.saturating_mul(8)))
        .and_then(|n| n.checked_add(65536));
    ensure!(
        minimum.is_some_and(|n| memory >= n) && memory <= 64 * 1024 * 1024 * 1024,
        "invalid journal memory reservation"
    );
    Ok((
        usize::try_from(meta)?,
        usize::try_from(request)?,
        usize::try_from(response)?,
        usize::try_from(memory)?,
        header[40..72].try_into().unwrap(),
    ))
}

fn header_prefix(meta: u64, request: u64, response: u64, memory: u64) -> [u8; 40] {
    let mut prefix = [0; 40];
    prefix[..8].copy_from_slice(MAGIC);
    for (index, value) in [meta, request, response, memory].iter().enumerate() {
        prefix[8 + index * 8..16 + index * 8].copy_from_slice(&value.to_le_bytes());
    }
    prefix
}

#[cfg(test)]
mod tests;
