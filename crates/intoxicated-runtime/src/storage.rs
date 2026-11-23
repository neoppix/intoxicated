//! One line at exit about the engine's own database, `rbx-storage.db`.
//!
//! The engine logs whether `RbxStorage` initialised before its log file is
//! open, so that line never reaches disk. This looks at the database from the
//! outside instead and says what is there: its size, its page count and how
//! many rows its tables hold. It is a report, not a verdict.
//!
//! No SQLite library is involved. The file is read once and its b-tree pages
//! are counted from the format's own header fields:
//! <https://www.sqlite.org/fileformat2.html>. Free and overflow pages are not
//! told apart from b-tree pages, so a free page whose first byte happens to be
//! a table leaf's type would be counted, and rows still only in the WAL are
//! not (INFERRED limits, kept on purpose: this is a count, not a check).

use std::path::Path;

/// The first 16 bytes of every SQLite 3 database.
const MAGIC: &[u8; 16] = b"SQLite format 3\0";
/// The database header's length, which is also where page 1's b-tree header starts.
const HEADER: usize = 100;
/// A table b-tree leaf page, the only page type whose cells are rows.
const TABLE_LEAF: u8 = 0x0d;

/// What [`inspect`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Inspection {
    /// No file at the path.
    Missing,
    /// A file that could not be read as a database, and why.
    Unreadable(&'static str),
    /// A database: its length in bytes, the header's page count (even when
    /// the file is shorter) and the rows across every table, SQLite's own
    /// `sqlite_master` and `sqlite_stat1` included.
    Present { bytes: u64, pages: u64, rows: u64 },
}

fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Read and count the database at `path`.
pub fn inspect(path: &Path) -> Inspection {
    match std::fs::read(path) {
        Ok(bytes) => count(&bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Inspection::Missing,
        Err(_) => Inspection::Unreadable("could not be read"),
    }
}

/// Count the rows in a database's bytes.
fn count(file: &[u8]) -> Inspection {
    if file.len() < HEADER || &file[..16] != MAGIC {
        return Inspection::Unreadable("not a SQLite database");
    }
    let page_size: usize = match be16(file, 16) {
        1 => 65536,
        n if n >= 512 && n.is_power_of_two() => n as usize,
        _ => return Inspection::Unreadable("implausible page size"),
    };
    let pages = be32(file, 28);
    let mut rows = 0u64;
    for page in 1..=u64::from(pages) {
        let start = if page == 1 {
            HEADER
        } else {
            match usize::try_from(page - 1).ok().and_then(|p| p.checked_mul(page_size)) {
                Some(s) => s,
                None => return Inspection::Unreadable("page count overflows the file"),
            }
        };
        // A truncated file: report what was counted.
        if start.checked_add(5).is_none_or(|end| end > file.len()) {
            break;
        }
        if file[start] == TABLE_LEAF {
            rows += u64::from(be16(file, start + 3));
        }
    }
    Inspection::Present { bytes: file.len() as u64, pages: u64::from(pages), rows }
}

/// Print one line about `<files_dir>/appData/rbx-storage.db`.
pub fn report(files_dir: &Path) {
    let path = files_dir.join("appData/rbx-storage.db");
    match inspect(&path) {
        Inspection::Present { bytes, pages, rows } => println!(
            "[storage] rbx-storage.db: {bytes} bytes, {pages} pages, {rows} rows across its tables ({})",
            path.display()
        ),
        Inspection::Missing => println!(
            "[storage] no rbx-storage.db at {} -- the engine's own RbxStorage::init line logs before its log \
             file is open, so this is the only report of it there is",
            path.display()
        ),
        Inspection::Unreadable(reason) => println!("[storage] rbx-storage.db at {}: {reason}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two pages of 4096: a table leaf with 3 cells on page 1 and an index
    /// leaf with 9 on page 2, the header claiming `pages`.
    fn synthetic(pages: u32) -> Vec<u8> {
        let mut f = vec![0u8; 8192];
        f[..16].copy_from_slice(MAGIC);
        f[16..18].copy_from_slice(&4096u16.to_be_bytes());
        f[28..32].copy_from_slice(&pages.to_be_bytes());
        f[100] = TABLE_LEAF;
        f[103..105].copy_from_slice(&3u16.to_be_bytes());
        f[4096] = 0x0a;
        f[4099..4101].copy_from_slice(&9u16.to_be_bytes());
        f
    }

    #[test]
    fn only_table_leaf_cells_are_rows() {
        assert_eq!(count(&synthetic(2)), Inspection::Present { bytes: 8192, pages: 2, rows: 3 });
    }

    #[test]
    fn a_header_that_claims_more_pages_than_the_file_has_is_reported_as_claimed() {
        assert_eq!(count(&synthetic(9)), Inspection::Present { bytes: 8192, pages: 9, rows: 3 });
    }

    #[test]
    fn text_is_not_a_database() {
        let short = b"hello, this is not sqlite".to_vec();
        let long = "not sqlite at all. ".repeat(20).into_bytes();
        assert_eq!(count(&short), Inspection::Unreadable("not a SQLite database"));
        assert_eq!(count(&long), Inspection::Unreadable("not a SQLite database"));
    }

    #[test]
    fn a_page_size_that_is_not_a_power_of_two_is_refused() {
        let mut f = synthetic(2);
        f[16..18].copy_from_slice(&1000u16.to_be_bytes());
        assert_eq!(count(&f), Inspection::Unreadable("implausible page size"));
        f[16..18].copy_from_slice(&256u16.to_be_bytes());
        assert_eq!(count(&f), Inspection::Unreadable("implausible page size"));
        // 1 is how the format writes 65536.
        f[16..18].copy_from_slice(&1u16.to_be_bytes());
        assert_eq!(count(&f), Inspection::Present { bytes: 8192, pages: 2, rows: 3 });
    }

    #[test]
    fn a_missing_file_is_missing() {
        assert_eq!(inspect(Path::new("/nonexistent/rbx-storage.db")), Inspection::Missing);
    }

    #[test]
    fn a_file_on_disk_is_read_once_and_counted() {
        let dir = std::env::temp_dir().join(format!("intoxicated-storage-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("rbx-storage.db");
        std::fs::write(&path, synthetic(2)).unwrap();
        assert_eq!(inspect(&path), Inspection::Present { bytes: 8192, pages: 2, rows: 3 });
        // A directory where the file should be cannot be read as one.
        assert_eq!(inspect(&dir), Inspection::Unreadable("could not be read"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
