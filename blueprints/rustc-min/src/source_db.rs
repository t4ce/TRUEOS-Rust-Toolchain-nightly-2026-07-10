use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use rusqlite_fork::{Connection, MAIN_DB, OptionalExtension, params};
use sha2::{Digest, Sha256};

pub const DEFAULT_DATABASE_PATH: &str = "rustc-sources.sqlite3";
pub const DEFAULT_FILE_MODE: u32 = 0o100644;

const SCHEMA_VERSION: i64 = 1;
const MAX_PROJECT_NAME_BYTES: usize = 128;
const MAX_SOURCE_LABEL_BYTES: usize = 512;
const MAX_SOURCE_PATH_BYTES: usize = 4096;

const REVISION_COLUMNS: &str = "
    select
        r.id,
        p.name,
        r.revision,
        r.origin_kind,
        r.repo_url,
        r.commit_sha,
        r.source_label,
        (select count(*) from source_file f where f.revision_id = r.id),
        (
            select coalesce(sum(b.byte_len), 0)
            from source_file f
            join source_blob b on b.id = f.blob_id
            where f.revision_id = r.id
        )
    from source_revision r
    join source_project p on p.id = r.project_id
";

const SCHEMA: &str = "
    pragma foreign_keys = on;

    create table if not exists source_project (
        id integer primary key,
        name text not null unique,
        next_revision integer not null default 1 check (next_revision > 0)
    );

    create table if not exists source_revision (
        id integer primary key,
        project_id integer not null
            references source_project(id) on delete cascade,
        revision integer not null check (revision > 0),
        origin_kind text not null check (origin_kind in ('git', 'local')),
        repo_url text,
        commit_sha text,
        source_label text,
        unique (project_id, revision),
        check (
            (
                origin_kind = 'git'
                and repo_url is not null
                and commit_sha is not null
            )
            or
            (
                origin_kind = 'local'
                and repo_url is null
                and commit_sha is null
            )
        )
    );

    create unique index if not exists source_git_commit_uq
        on source_revision(repo_url, commit_sha)
        where origin_kind = 'git';

    create table if not exists source_blob (
        id integer primary key,
        sha256 text not null unique check (length(sha256) = 64),
        byte_len integer not null check (byte_len >= 0),
        bytes blob not null,
        check (length(bytes) = byte_len)
    );

    create table if not exists source_file (
        revision_id integer not null
            references source_revision(id) on delete cascade,
        path text not null,
        blob_id integer not null references source_blob(id),
        mode integer not null check (mode >= 0 and mode <= 4294967295),
        primary key (revision_id, path)
    ) without rowid;

    pragma user_version = 1;
";

#[derive(Debug)]
pub enum SourceDbError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Sqlite(rusqlite_fork::Error),
    InvalidProjectName(String),
    InvalidSourceLabel(String),
    InvalidSourcePath(String),
    DuplicateSourcePath(String),
    EmptySourceSet,
    InvalidGitRepository(String),
    InvalidGitCommit(String),
    Integrity(String),
    UnsupportedSchemaVersion(i64),
    NumericRange(&'static str),
}

impl fmt::Display for SourceDbError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(formatter, "{operation} {}: {source}", path.display()),
            Self::Sqlite(error) => write!(formatter, "SQLite: {error}"),
            Self::InvalidProjectName(message) => {
                write!(formatter, "invalid project name: {message}")
            }
            Self::InvalidSourceLabel(message) => {
                write!(formatter, "invalid source label: {message}")
            }
            Self::InvalidSourcePath(path) => write!(formatter, "invalid source path `{path}`"),
            Self::DuplicateSourcePath(path) => write!(formatter, "duplicate source path `{path}`"),
            Self::EmptySourceSet => write!(formatter, "source set contains no files"),
            Self::InvalidGitRepository(message) => {
                write!(formatter, "invalid Git repository: {message}")
            }
            Self::InvalidGitCommit(message) => write!(formatter, "invalid Git commit: {message}"),
            Self::Integrity(message) => write!(formatter, "source database integrity: {message}"),
            Self::UnsupportedSchemaVersion(version) => {
                write!(
                    formatter,
                    "unsupported source database schema version {version}"
                )
            }
            Self::NumericRange(field) => {
                write!(formatter, "{field} is outside the supported range")
            }
        }
    }
}

impl std::error::Error for SourceDbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            Self::Sqlite(error) => Some(error),
            _ => None,
        }
    }
}

impl From<rusqlite_fork::Error> for SourceDbError {
    fn from(error: rusqlite_fork::Error) -> Self {
        Self::Sqlite(error)
    }
}

pub type Result<T> = std::result::Result<T, SourceDbError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFileInput {
    pub path: String,
    pub bytes: Vec<u8>,
    pub mode: u32,
}

impl SourceFileInput {
    pub fn new(path: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
            mode: DEFAULT_FILE_MODE,
        }
    }

    pub fn with_mode(path: impl Into<String>, bytes: impl Into<Vec<u8>>, mode: u32) -> Self {
        Self {
            path: path.into(),
            bytes: bytes.into(),
            mode,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub mode: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SourceOrigin {
    Local { label: Option<String> },
    Git { repository: String, commit: String },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RevisionSummary {
    pub id: i64,
    pub project: String,
    pub revision: u64,
    pub origin: SourceOrigin,
    pub file_count: u64,
    pub source_bytes: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectSummary {
    pub id: i64,
    pub name: String,
    pub revision_count: u64,
    pub latest_revision: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoadedSource {
    pub revision: RevisionSummary,
    pub files: Vec<SourceFile>,
}

impl LoadedSource {
    pub fn file(&self, path: &str) -> Option<&SourceFile> {
        self.files
            .binary_search_by(|file| file.path.as_str().cmp(path))
            .ok()
            .map(|index| &self.files[index])
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportDisposition {
    Inserted,
    AlreadyPresent,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportResult {
    pub disposition: ImportDisposition,
    pub revision: RevisionSummary,
}

struct RawRevision {
    id: i64,
    project: String,
    revision: i64,
    origin_kind: String,
    repository: Option<String>,
    commit: Option<String>,
    source_label: Option<String>,
    file_count: i64,
    source_bytes: i64,
}

impl RawRevision {
    fn into_summary(self) -> Result<RevisionSummary> {
        let origin = match self.origin_kind.as_str() {
            "local" if self.repository.is_none() && self.commit.is_none() => SourceOrigin::Local {
                label: self.source_label,
            },
            "git" => {
                let repository = self.repository.ok_or_else(|| {
                    SourceDbError::Integrity(String::from("Git revision has no repository URL"))
                })?;
                let commit = self.commit.ok_or_else(|| {
                    SourceDbError::Integrity(String::from("Git revision has no commit"))
                })?;
                SourceOrigin::Git { repository, commit }
            }
            other => {
                return Err(SourceDbError::Integrity(format!(
                    "revision {} has inconsistent origin `{other}`",
                    self.id
                )));
            }
        };
        Ok(RevisionSummary {
            id: self.id,
            project: self.project,
            revision: positive_i64_to_u64(self.revision, "revision")?,
            origin,
            file_count: nonnegative_i64_to_u64(self.file_count, "file count")?,
            source_bytes: nonnegative_i64_to_u64(self.source_bytes, "source byte count")?,
        })
    }
}

pub struct SourceDatabase {
    path: PathBuf,
    conn: Connection,
}

impl SourceDatabase {
    pub fn open_default() -> Result<Self> {
        Self::open(DEFAULT_DATABASE_PATH)
    }

    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let image = match fs::read(path.as_path()) {
            Ok(image) => Some(image),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(source) => {
                return Err(SourceDbError::Io {
                    operation: "read",
                    path,
                    source,
                });
            }
        };

        let mut conn = Connection::open_in_memory()?;
        if let Some(image) = image {
            if !image.is_empty() {
                conn.deserialize_read_exact(MAIN_DB, image.as_slice(), image.len(), false)?;
                quick_check(&conn)?;
            }
        }
        conn.execute_batch("pragma foreign_keys = on;")?;
        let version: i64 = conn.query_row("pragma user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(SourceDbError::UnsupportedSchemaVersion(version));
        }
        conn.execute_batch(SCHEMA)?;
        quick_check(&conn)?;
        Ok(Self { path, conn })
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    pub fn verify(&self) -> Result<()> {
        quick_check(&self.conn)
    }

    pub fn find_git(&self, repository: &str, commit: &str) -> Result<Option<RevisionSummary>> {
        let repository = canonicalize_git_repository(repository)?;
        let commit = canonicalize_git_commit(commit)?;
        self.find_git_canonical(repository.as_str(), commit.as_str())
    }

    pub fn import_git(
        &mut self,
        project: &str,
        repository: &str,
        commit: &str,
        files: Vec<SourceFileInput>,
    ) -> Result<ImportResult> {
        let project = normalize_project_name(project)?;
        let repository = canonicalize_git_repository(repository)?;
        let commit = canonicalize_git_commit(commit)?;
        if let Some(revision) = self.find_git_canonical(repository.as_str(), commit.as_str())? {
            return Ok(ImportResult {
                disposition: ImportDisposition::AlreadyPresent,
                revision,
            });
        }
        let files = normalize_file_set(files)?;
        let transaction = self.conn.transaction()?;
        let (project_id, revision) =
            ensure_project_and_allocate_revision(&transaction, project.as_str())?;
        transaction.execute(
            "insert into source_revision (
                 project_id, revision, origin_kind, repo_url, commit_sha, source_label
             ) values (?1, ?2, 'git', ?3, ?4, null)",
            params![project_id, revision, repository.as_str(), commit.as_str()],
        )?;
        let id = transaction.last_insert_rowid();
        insert_file_set(&transaction, id, files.as_slice())?;
        advance_project_revision(&transaction, project_id, revision)?;
        transaction.commit()?;
        self.persist_verified()?;
        Ok(ImportResult {
            disposition: ImportDisposition::Inserted,
            revision: self.require_revision(id)?,
        })
    }

    pub fn import_local(
        &mut self,
        project: &str,
        source_label: Option<&str>,
        files: Vec<SourceFileInput>,
    ) -> Result<ImportResult> {
        let project = normalize_project_name(project)?;
        let source_label = normalize_source_label(source_label)?;
        let files = normalize_file_set(files)?;
        let transaction = self.conn.transaction()?;
        let (project_id, revision) =
            ensure_project_and_allocate_revision(&transaction, project.as_str())?;
        transaction.execute(
            "insert into source_revision (
                 project_id, revision, origin_kind, repo_url, commit_sha, source_label
             ) values (?1, ?2, 'local', null, null, ?3)",
            params![project_id, revision, source_label],
        )?;
        let id = transaction.last_insert_rowid();
        insert_file_set(&transaction, id, files.as_slice())?;
        advance_project_revision(&transaction, project_id, revision)?;
        transaction.commit()?;
        self.persist_verified()?;
        Ok(ImportResult {
            disposition: ImportDisposition::Inserted,
            revision: self.require_revision(id)?,
        })
    }

    pub fn list_projects(&self) -> Result<Vec<ProjectSummary>> {
        let mut statement = self.conn.prepare(
            "select p.id, p.name, count(r.id), max(r.revision)
             from source_project p
             left join source_revision r on r.project_id = p.id
             group by p.id, p.name
             order by lower(p.name), p.name",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })?;
        let mut projects = Vec::new();
        for row in rows {
            let (id, name, count, latest) = row?;
            projects.push(ProjectSummary {
                id,
                name,
                revision_count: nonnegative_i64_to_u64(count, "revision count")?,
                latest_revision: latest
                    .map(|value| positive_i64_to_u64(value, "latest revision"))
                    .transpose()?,
            });
        }
        Ok(projects)
    }

    pub fn list_revisions(&self, project: &str) -> Result<Vec<RevisionSummary>> {
        let project = normalize_project_name(project)?;
        let sql = format!(
            "{REVISION_COLUMNS}
             where p.name = ?1
             order by r.revision"
        );
        let mut statement = self.conn.prepare(sql.as_str())?;
        let rows = statement.query_map(params![project], raw_revision_from_row)?;
        let mut revisions = Vec::new();
        for row in rows {
            revisions.push(row?.into_summary()?);
        }
        Ok(revisions)
    }

    pub fn get(&self, project: &str, revision: Option<u64>) -> Result<Option<LoadedSource>> {
        let project = normalize_project_name(project)?;
        let raw = match revision {
            Some(revision) => {
                let revision =
                    i64::try_from(revision).map_err(|_| SourceDbError::NumericRange("revision"))?;
                let sql = format!(
                    "{REVISION_COLUMNS}
                     where p.name = ?1 and r.revision = ?2"
                );
                self.conn
                    .query_row(
                        sql.as_str(),
                        params![project, revision],
                        raw_revision_from_row,
                    )
                    .optional()?
            }
            None => {
                let sql = format!(
                    "{REVISION_COLUMNS}
                     where p.name = ?1
                     order by r.revision desc limit 1"
                );
                self.conn
                    .query_row(sql.as_str(), params![project], raw_revision_from_row)
                    .optional()?
            }
        };
        raw.map(|raw| self.load_raw(raw)).transpose()
    }

    pub fn get_by_id(&self, id: i64) -> Result<Option<LoadedSource>> {
        self.raw_revision_by_id(id)?
            .map(|raw| self.load_raw(raw))
            .transpose()
    }

    pub fn delete_revision(&mut self, project: &str, revision: u64) -> Result<bool> {
        let project = normalize_project_name(project)?;
        let revision =
            i64::try_from(revision).map_err(|_| SourceDbError::NumericRange("revision"))?;
        let transaction = self.conn.transaction()?;
        let deleted = transaction.execute(
            "delete from source_revision
             where project_id = (select id from source_project where name = ?1)
               and revision = ?2",
            params![project, revision],
        )? > 0;
        if !deleted {
            transaction.rollback()?;
            return Ok(false);
        }
        prune_unreferenced_blobs(&transaction)?;
        transaction.commit()?;
        self.persist_verified()?;
        Ok(true)
    }

    pub fn delete_project(&mut self, project: &str) -> Result<bool> {
        let project = normalize_project_name(project)?;
        let transaction = self.conn.transaction()?;
        let deleted = transaction.execute(
            "delete from source_project where name = ?1",
            params![project],
        )? > 0;
        if !deleted {
            transaction.rollback()?;
            return Ok(false);
        }
        prune_unreferenced_blobs(&transaction)?;
        transaction.commit()?;
        self.persist_verified()?;
        Ok(true)
    }

    fn find_git_canonical(
        &self,
        repository: &str,
        commit: &str,
    ) -> Result<Option<RevisionSummary>> {
        let sql = format!(
            "{REVISION_COLUMNS}
             where r.origin_kind = 'git' and r.repo_url = ?1 and r.commit_sha = ?2"
        );
        self.conn
            .query_row(
                sql.as_str(),
                params![repository, commit],
                raw_revision_from_row,
            )
            .optional()?
            .map(RawRevision::into_summary)
            .transpose()
    }

    fn require_revision(&self, id: i64) -> Result<RevisionSummary> {
        self.raw_revision_by_id(id)?
            .ok_or_else(|| SourceDbError::Integrity(format!("inserted revision {id} is missing")))?
            .into_summary()
    }

    fn raw_revision_by_id(&self, id: i64) -> Result<Option<RawRevision>> {
        let sql = format!("{REVISION_COLUMNS} where r.id = ?1");
        Ok(self
            .conn
            .query_row(sql.as_str(), params![id], raw_revision_from_row)
            .optional()?)
    }

    fn load_raw(&self, raw: RawRevision) -> Result<LoadedSource> {
        let revision = raw.into_summary()?;
        let mut statement = self.conn.prepare(
            "select f.path, f.mode, b.sha256, b.byte_len, b.bytes
             from source_file f
             join source_blob b on b.id = f.blob_id
             where f.revision_id = ?1
             order by f.path",
        )?;
        let rows = statement.query_map(params![revision.id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, Vec<u8>>(4)?,
            ))
        })?;
        let mut files = Vec::new();
        let mut source_bytes = 0u64;
        for row in rows {
            let (path, mode, digest, byte_len, bytes) = row?;
            let mode = u32::try_from(mode).map_err(|_| SourceDbError::NumericRange("file mode"))?;
            validate_stored_blob(path.as_str(), digest.as_str(), byte_len, bytes.as_slice())?;
            source_bytes = source_bytes
                .checked_add(
                    u64::try_from(bytes.len())
                        .map_err(|_| SourceDbError::NumericRange("source bytes"))?,
                )
                .ok_or(SourceDbError::NumericRange("source bytes"))?;
            files.push(SourceFile { path, bytes, mode });
        }
        if files.len() as u64 != revision.file_count || source_bytes != revision.source_bytes {
            return Err(SourceDbError::Integrity(format!(
                "revision {} file summary does not match its path map",
                revision.id
            )));
        }
        Ok(LoadedSource { revision, files })
    }

    fn persist_verified(&self) -> Result<()> {
        quick_check(&self.conn)?;
        let image = {
            let data = self.conn.serialize(MAIN_DB)?;
            data.to_vec()
        };
        fs::write(self.path.as_path(), image.as_slice()).map_err(|source| SourceDbError::Io {
            operation: "write",
            path: self.path.clone(),
            source,
        })?;
        let persisted = fs::read(self.path.as_path()).map_err(|source| SourceDbError::Io {
            operation: "read back",
            path: self.path.clone(),
            source,
        })?;
        if persisted != image {
            return Err(SourceDbError::Integrity(format!(
                "persisted image readback mismatch at {}",
                self.path.display()
            )));
        }
        verify_image(persisted.as_slice())
    }
}

pub fn canonicalize_git_repository(repository: &str) -> Result<String> {
    let repository = repository.trim();
    if repository.is_empty() || repository.bytes().any(|byte| byte.is_ascii_whitespace()) {
        return Err(SourceDbError::InvalidGitRepository(String::from(
            "URL is empty or contains whitespace",
        )));
    }
    if repository.contains('?') || repository.contains('#') {
        return Err(SourceDbError::InvalidGitRepository(String::from(
            "repository URL must not contain a query or fragment",
        )));
    }
    let (scheme, remainder) = repository.split_once("://").ok_or_else(|| {
        SourceDbError::InvalidGitRepository(String::from("expected an HTTPS URL"))
    })?;
    if !scheme.eq_ignore_ascii_case("https") {
        return Err(SourceDbError::InvalidGitRepository(String::from(
            "only HTTPS repository URLs are supported",
        )));
    }
    let (authority, path) = remainder.split_once('/').ok_or_else(|| {
        SourceDbError::InvalidGitRepository(String::from("repository path is missing"))
    })?;
    if authority.is_empty() || authority.contains('@') || authority.contains('\\') {
        return Err(SourceDbError::InvalidGitRepository(String::from(
            "repository host is invalid",
        )));
    }
    let mut components = Vec::new();
    for component in path.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }
        if component == ".." || component.contains('\\') {
            return Err(SourceDbError::InvalidGitRepository(String::from(
                "repository path is invalid",
            )));
        }
        components.push(component);
    }
    let last = components.last_mut().ok_or_else(|| {
        SourceDbError::InvalidGitRepository(String::from("repository path is missing"))
    })?;
    if last
        .get(last.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(".git"))
    {
        *last = &last[..last.len() - 4];
    }
    if last.is_empty() {
        return Err(SourceDbError::InvalidGitRepository(String::from(
            "repository name is missing",
        )));
    }
    Ok(format!(
        "https://{}/{}",
        authority.to_ascii_lowercase(),
        components.join("/")
    ))
}

pub fn canonicalize_git_commit(commit: &str) -> Result<String> {
    let commit = commit.trim();
    if !matches!(commit.len(), 40 | 64) || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(SourceDbError::InvalidGitCommit(String::from(
            "expected a full 40- or 64-character hexadecimal object ID",
        )));
    }
    Ok(commit.to_ascii_lowercase())
}

fn normalize_project_name(project: &str) -> Result<String> {
    let project = project.trim();
    if project.is_empty()
        || project.len() > MAX_PROJECT_NAME_BYTES
        || project.chars().any(char::is_control)
    {
        return Err(SourceDbError::InvalidProjectName(format!(
            "expected 1-{MAX_PROJECT_NAME_BYTES} bytes without control characters"
        )));
    }
    Ok(project.to_owned())
}

fn normalize_source_label(label: Option<&str>) -> Result<Option<String>> {
    let Some(label) = label else {
        return Ok(None);
    };
    let label = label.trim();
    if label.is_empty() {
        return Ok(None);
    }
    if label.len() > MAX_SOURCE_LABEL_BYTES || label.chars().any(char::is_control) {
        return Err(SourceDbError::InvalidSourceLabel(format!(
            "expected at most {MAX_SOURCE_LABEL_BYTES} bytes without control characters"
        )));
    }
    Ok(Some(label.to_owned()))
}

fn normalize_file_set(mut files: Vec<SourceFileInput>) -> Result<Vec<SourceFileInput>> {
    if files.is_empty() {
        return Err(SourceDbError::EmptySourceSet);
    }
    for file in &mut files {
        file.path = normalize_source_path(file.path.as_str())?;
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    for pair in files.windows(2) {
        if pair[0].path == pair[1].path {
            return Err(SourceDbError::DuplicateSourcePath(pair[0].path.clone()));
        }
    }
    Ok(files)
}

fn normalize_source_path(path: &str) -> Result<String> {
    let path = path.trim();
    if path.is_empty()
        || path.len() > MAX_SOURCE_PATH_BYTES
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path.chars().any(char::is_control)
    {
        return Err(SourceDbError::InvalidSourcePath(path.to_owned()));
    }
    let mut normalized = Vec::new();
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(SourceDbError::InvalidSourcePath(path.to_owned()));
        }
        normalized.push(component);
    }
    Ok(normalized.join("/"))
}

fn raw_revision_from_row(row: &rusqlite_fork::Row<'_>) -> rusqlite_fork::Result<RawRevision> {
    Ok(RawRevision {
        id: row.get(0)?,
        project: row.get(1)?,
        revision: row.get(2)?,
        origin_kind: row.get(3)?,
        repository: row.get(4)?,
        commit: row.get(5)?,
        source_label: row.get(6)?,
        file_count: row.get(7)?,
        source_bytes: row.get(8)?,
    })
}

fn ensure_project_and_allocate_revision(
    transaction: &rusqlite_fork::Transaction<'_>,
    project: &str,
) -> Result<(i64, i64)> {
    transaction.execute(
        "insert into source_project(name, next_revision)
         values (?1, 1) on conflict(name) do nothing",
        params![project],
    )?;
    let result = transaction.query_row(
        "select id, next_revision from source_project where name = ?1",
        params![project],
        |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
    )?;
    if result.1 <= 0 {
        return Err(SourceDbError::Integrity(format!(
            "project `{project}` has invalid next revision {}",
            result.1
        )));
    }
    Ok(result)
}

fn advance_project_revision(
    transaction: &rusqlite_fork::Transaction<'_>,
    project_id: i64,
    revision: i64,
) -> Result<()> {
    let changed = transaction.execute(
        "update source_project set next_revision = ?2 + 1
         where id = ?1 and next_revision = ?2",
        params![project_id, revision],
    )?;
    if changed != 1 {
        return Err(SourceDbError::Integrity(format!(
            "failed to advance revision counter for project {project_id}"
        )));
    }
    Ok(())
}

fn insert_file_set(
    transaction: &rusqlite_fork::Transaction<'_>,
    revision_id: i64,
    files: &[SourceFileInput],
) -> Result<()> {
    for file in files {
        let digest = sha256_hex(file.bytes.as_slice());
        let byte_len = i64::try_from(file.bytes.len())
            .map_err(|_| SourceDbError::NumericRange("source file"))?;
        transaction.execute(
            "insert into source_blob(sha256, byte_len, bytes)
             values (?1, ?2, ?3) on conflict(sha256) do nothing",
            params![digest.as_str(), byte_len, file.bytes.as_slice()],
        )?;
        let (blob_id, stored_len, stored_bytes) = transaction.query_row(
            "select id, byte_len, bytes from source_blob where sha256 = ?1",
            params![digest.as_str()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Vec<u8>>(2)?,
                ))
            },
        )?;
        if stored_len != byte_len || stored_bytes != file.bytes {
            return Err(SourceDbError::Integrity(format!(
                "source blob digest collision for {digest}"
            )));
        }
        transaction.execute(
            "insert into source_file(revision_id, path, blob_id, mode)
             values (?1, ?2, ?3, ?4)",
            params![
                revision_id,
                file.path.as_str(),
                blob_id,
                i64::from(file.mode)
            ],
        )?;
    }
    Ok(())
}

fn prune_unreferenced_blobs(transaction: &rusqlite_fork::Transaction<'_>) -> Result<()> {
    transaction.execute(
        "delete from source_blob where not exists (
             select 1 from source_file where source_file.blob_id = source_blob.id
         )",
        [],
    )?;
    Ok(())
}

fn validate_stored_blob(path: &str, digest: &str, byte_len: i64, bytes: &[u8]) -> Result<()> {
    normalize_source_path(path)
        .map_err(|_| SourceDbError::Integrity(format!("stored source path `{path}` is invalid")))?;
    let expected_len = nonnegative_i64_to_u64(byte_len, "stored source length")?;
    let actual_len =
        u64::try_from(bytes.len()).map_err(|_| SourceDbError::NumericRange("source file"))?;
    if expected_len != actual_len || sha256_hex(bytes) != digest {
        return Err(SourceDbError::Integrity(format!(
            "stored source blob for `{path}` failed length or digest verification"
        )));
    }
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut value = String::with_capacity(64);
    for byte in digest {
        value.push(HEX[usize::from(byte >> 4)] as char);
        value.push(HEX[usize::from(byte & 0x0f)] as char);
    }
    value
}

fn quick_check(conn: &Connection) -> Result<()> {
    let result: String = conn.query_row("pragma quick_check", [], |row| row.get(0))?;
    if result == "ok" {
        Ok(())
    } else {
        Err(SourceDbError::Integrity(format!(
            "SQLite quick_check failed: {result}"
        )))
    }
}

fn verify_image(image: &[u8]) -> Result<()> {
    if image.is_empty() {
        return Err(SourceDbError::Integrity(String::from(
            "persisted SQLite image is empty",
        )));
    }
    let mut conn = Connection::open_in_memory()?;
    conn.deserialize_read_exact(MAIN_DB, image, image.len(), false)?;
    quick_check(&conn)?;
    let version: i64 = conn.query_row("pragma user_version", [], |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        return Err(SourceDbError::Integrity(format!(
            "persisted schema version is {version}, expected {SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

fn positive_i64_to_u64(value: i64, field: &'static str) -> Result<u64> {
    if value <= 0 {
        return Err(SourceDbError::Integrity(format!(
            "{field} must be positive, got {value}"
        )));
    }
    u64::try_from(value).map_err(|_| SourceDbError::NumericRange(field))
}

fn nonnegative_i64_to_u64(value: i64, field: &'static str) -> Result<u64> {
    if value < 0 {
        return Err(SourceDbError::Integrity(format!(
            "{field} must be nonnegative, got {value}"
        )));
    }
    u64::try_from(value).map_err(|_| SourceDbError::NumericRange(field))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new() -> Self {
            let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "trueos-rustc-source-db-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir_all(path.as_path()).unwrap();
            Self(path)
        }

        fn database_path(&self) -> PathBuf {
            self.0.join("sources.sqlite3")
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(self.0.as_path());
        }
    }

    fn source(path: &str, bytes: &[u8]) -> SourceFileInput {
        SourceFileInput::new(path, bytes)
    }

    fn commit(byte: u8) -> String {
        format!("{byte:02x}").repeat(20)
    }

    #[test]
    fn canonicalizes_git_identity_and_requires_exact_commit() {
        assert_eq!(
            canonicalize_git_repository(" HTTPS://GitHub.COM/TrueOS/Example.GIT/ ").unwrap(),
            "https://github.com/TrueOS/Example"
        );
        assert_eq!(
            canonicalize_git_commit(commit(0xab).to_ascii_uppercase().as_str()).unwrap(),
            commit(0xab)
        );
        assert!(canonicalize_git_repository("http://github.com/a/b").is_err());
        assert!(canonicalize_git_commit("deadbeef").is_err());
    }

    #[test]
    fn git_import_is_idempotent_and_preserves_original_file_bytes() {
        let temp = TestDirectory::new();
        let mut database = SourceDatabase::open(temp.database_path()).unwrap();
        let files = vec![
            source("Cargo.toml", b"[package]\nname='demo'\n"),
            SourceFileInput::with_mode("src/main.rs", b"fn main() {}\n", 0o100755),
        ];
        let imported = database
            .import_git(
                "demo",
                "https://github.com/TRUEOS/demo.git",
                commit(0x11).as_str(),
                files.clone(),
            )
            .unwrap();
        let repeated = database
            .import_git(
                "ignored",
                "HTTPS://GITHUB.COM/TRUEOS/demo/",
                commit(0x11).as_str(),
                vec![source("changed", b"must not replace")],
            )
            .unwrap();
        assert_eq!(repeated.disposition, ImportDisposition::AlreadyPresent);
        assert_eq!(repeated.revision.id, imported.revision.id);

        let loaded = database.get_by_id(imported.revision.id).unwrap().unwrap();
        assert_eq!(loaded.file("src/main.rs").unwrap().bytes, b"fn main() {}\n");
        assert_eq!(loaded.file("src/main.rs").unwrap().mode, 0o100755);
        assert_eq!(loaded.files.len(), 2);
    }

    #[test]
    fn local_revisions_deduplicate_blobs_and_keep_path_maps() {
        let temp = TestDirectory::new();
        let mut database = SourceDatabase::open(temp.database_path()).unwrap();
        let common = b"same exact bytes";
        let first = database
            .import_local(
                "app",
                Some("first.7z"),
                vec![source("src/main.rs", common), source("Cargo.toml", b"one")],
            )
            .unwrap();
        let second = database
            .import_local(
                "app",
                Some("second.7z"),
                vec![source("src/lib.rs", common), source("Cargo.toml", b"two")],
            )
            .unwrap();
        assert_eq!(first.revision.revision, 1);
        assert_eq!(second.revision.revision, 2);
        let blob_count: i64 = database
            .conn
            .query_row("select count(*) from source_blob", [], |row| row.get(0))
            .unwrap();
        assert_eq!(blob_count, 3);
        assert_eq!(
            database
                .get("app", Some(1))
                .unwrap()
                .unwrap()
                .file("src/main.rs")
                .unwrap()
                .bytes,
            common
        );
        assert_eq!(
            database
                .get("app", None)
                .unwrap()
                .unwrap()
                .file("src/lib.rs")
                .unwrap()
                .bytes,
            common
        );
    }

    #[test]
    fn deletion_prunes_orphans_and_does_not_reuse_revision_numbers() {
        let temp = TestDirectory::new();
        let mut database = SourceDatabase::open(temp.database_path()).unwrap();
        database
            .import_local("app", None, vec![source("one", b"shared")])
            .unwrap();
        database
            .import_local("app", None, vec![source("two", b"shared")])
            .unwrap();
        assert!(database.delete_revision("app", 1).unwrap());
        let blob_count: i64 = database
            .conn
            .query_row("select count(*) from source_blob", [], |row| row.get(0))
            .unwrap();
        assert_eq!(blob_count, 1);
        let third = database
            .import_local("app", None, vec![source("three", b"new")])
            .unwrap();
        assert_eq!(third.revision.revision, 3);
        assert!(database.delete_project("app").unwrap());
        let blob_count: i64 = database
            .conn
            .query_row("select count(*) from source_blob", [], |row| row.get(0))
            .unwrap();
        assert_eq!(blob_count, 0);
    }

    #[test]
    fn persists_reopens_and_rejects_corrupt_images() {
        let temp = TestDirectory::new();
        let path = temp.database_path();
        {
            let mut database = SourceDatabase::open(path.as_path()).unwrap();
            database
                .import_local(
                    "persisted",
                    Some("input.7z"),
                    vec![source("src/main.rs", b"fn main() {}")],
                )
                .unwrap();
            database.verify().unwrap();
        }
        assert_eq!(
            SourceDatabase::open(path.as_path())
                .unwrap()
                .get("persisted", Some(1))
                .unwrap()
                .unwrap()
                .files[0]
                .bytes,
            b"fn main() {}"
        );
        fs::write(path.as_path(), b"not SQLite").unwrap();
        assert!(SourceDatabase::open(path.as_path()).is_err());
    }

    #[test]
    fn rejects_unsafe_or_duplicate_paths_without_persisting() {
        let temp = TestDirectory::new();
        let path = temp.database_path();
        let mut database = SourceDatabase::open(path.as_path()).unwrap();
        assert!(
            database
                .import_local("bad", None, vec![source("../escape", b"x")])
                .is_err()
        );
        assert!(
            database
                .import_local(
                    "bad",
                    None,
                    vec![source("src/main.rs", b"a"), source("src/main.rs", b"b")]
                )
                .is_err()
        );
        assert!(database.list_projects().unwrap().is_empty());
        assert!(!path.exists());
    }
}
