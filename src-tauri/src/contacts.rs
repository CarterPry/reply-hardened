use rusqlite::{Connection, OpenFlags};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Resolves iMessage handles (phone numbers, emails) to display names
/// by reading the local AddressBook SQLite stores. Best-effort: any
/// failure (missing file, FDA blocked, schema variation) yields an
/// empty directory; the UI then falls back to the raw handle.
#[derive(Default, Debug, Clone)]
pub struct ContactDirectory {
    /// Last 10 digits of a phone → display name. Matching by tail
    /// sidesteps country-code mismatch between AddressBook and iMessage.
    by_phone_tail: HashMap<String, String>,
    /// Lowercased email → display name.
    by_email: HashMap<String, String>,
}

impl ContactDirectory {
    pub fn lookup(&self, handle: &str) -> Option<&str> {
        let h = handle.trim();
        if h.is_empty() {
            return None;
        }
        if h.contains('@') {
            self.by_email.get(&h.to_lowercase()).map(String::as_str)
        } else {
            let key = phone_tail(h);
            if key.is_empty() {
                None
            } else {
                self.by_phone_tail.get(&key).map(String::as_str)
            }
        }
    }
}

fn phone_tail(s: &str) -> String {
    let digits: String = s.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() <= 10 {
        digits
    } else {
        digits[digits.len() - 10..].to_string()
    }
}

pub fn load() -> ContactDirectory {
    let mut dir = ContactDirectory::default();
    let Ok(home) = std::env::var("HOME") else {
        return dir;
    };
    let root = PathBuf::from(home).join("Library/Application Support/AddressBook");
    if !root.exists() {
        return dir;
    }

    let mut dbs = Vec::new();
    walk_for_abcddb(&root, &mut dbs);

    for db_path in dbs {
        if let Err(e) = read_one(&db_path, &mut dir) {
            log::warn!("addressbook read {} failed: {}", db_path.display(), e);
        }
    }
    dir
}

fn walk_for_abcddb(start: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(start) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_for_abcddb(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("abcddb") {
            out.push(path);
        }
    }
}

fn read_one(path: &Path, dir: &mut ContactDirectory) -> Result<(), rusqlite::Error> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;

    let mut names: HashMap<i64, String> = HashMap::new();
    {
        let mut stmt = conn.prepare(
            "SELECT Z_PK, ZFIRSTNAME, ZLASTNAME, ZNICKNAME, ZORGANIZATION FROM ZABCDRECORD",
        )?;
        let rows = stmt.query_map([], |row| {
            let pk: i64 = row.get(0)?;
            let first: Option<String> = row.get(1)?;
            let last: Option<String> = row.get(2)?;
            let nick: Option<String> = row.get(3)?;
            let org: Option<String> = row.get(4)?;
            Ok((pk, build_name(first, last, nick, org)))
        })?;
        for r in rows.flatten() {
            if let (pk, Some(name)) = r {
                names.insert(pk, name);
            }
        }
    }

    {
        let mut stmt = conn.prepare(
            "SELECT ZOWNER, ZFULLNUMBER FROM ZABCDPHONENUMBER WHERE ZFULLNUMBER IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |row| {
            let owner: i64 = row.get(0)?;
            let phone: String = row.get(1)?;
            Ok((owner, phone))
        })?;
        for r in rows.flatten() {
            if let Some(name) = names.get(&r.0) {
                let key = phone_tail(&r.1);
                if !key.is_empty() {
                    dir.by_phone_tail.entry(key).or_insert_with(|| name.clone());
                }
            }
        }
    }

    {
        let mut stmt = conn
            .prepare("SELECT ZOWNER, ZADDRESS FROM ZABCDEMAILADDRESS WHERE ZADDRESS IS NOT NULL")?;
        let rows = stmt.query_map([], |row| {
            let owner: i64 = row.get(0)?;
            let email: String = row.get(1)?;
            Ok((owner, email))
        })?;
        for r in rows.flatten() {
            if let Some(name) = names.get(&r.0) {
                let key = r.1.trim().to_lowercase();
                if !key.is_empty() {
                    dir.by_email.entry(key).or_insert_with(|| name.clone());
                }
            }
        }
    }

    Ok(())
}

fn build_name(
    first: Option<String>,
    last: Option<String>,
    nick: Option<String>,
    org: Option<String>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(f) = first.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        parts.push(f.to_string());
    }
    if let Some(l) = last.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        parts.push(l.to_string());
    }
    if !parts.is_empty() {
        return Some(parts.join(" "));
    }
    if let Some(n) = nick.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        return Some(n.to_string());
    }
    if let Some(o) = org.as_ref().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        return Some(o.to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phone_tail_trims_to_last_10() {
        assert_eq!(phone_tail("+1 (415) 555-0100"), "4155550100");
        assert_eq!(phone_tail("415-555-0100"), "4155550100");
        assert_eq!(phone_tail("5551234"), "5551234");
        assert_eq!(phone_tail(""), "");
    }
}
