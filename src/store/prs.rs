use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use rusqlite::{Row, params};
use tracing::warn;

use super::Db;
use crate::domain::model::{MyPr, ReviewRequest};
use crate::domain::reasons::Reason;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Review,
    Mine,
}

impl Tab {
    pub fn as_str(self) -> &'static str {
        match self {
            Tab::Review => "review",
            Tab::Mine => "mine",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrRow {
    pub account: String,
    pub tab: Tab,
    pub key: String,
    pub repo: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: String,
    pub is_draft: bool,
    pub reasons: Vec<Reason>,
    pub team: Option<String>,
    pub is_direct: bool,
    pub last_activity_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl PrRow {
    pub fn mine(account: &str, pr: &MyPr, reasons: Vec<Reason>, last_activity_at: Option<DateTime<Utc>>) -> Self {
        let b = &pr.base;
        PrRow {
            account: account.to_string(),
            tab: Tab::Mine,
            key: b.key(),
            repo: b.repo.clone(),
            number: b.number,
            title: b.title.clone(),
            url: b.url.clone(),
            author: b.author.clone(),
            is_draft: b.is_draft,
            reasons,
            team: None,
            is_direct: true,
            last_activity_at,
            created_at: b.created_at,
        }
    }

    pub fn review(account: &str, rr: &ReviewRequest) -> Self {
        let b = &rr.base;
        PrRow {
            account: account.to_string(),
            tab: Tab::Review,
            key: b.key(),
            repo: b.repo.clone(),
            number: b.number,
            title: b.title.clone(),
            url: b.url.clone(),
            author: b.author.clone(),
            is_draft: b.is_draft,
            reasons: vec![],
            team: rr.team.clone(),
            is_direct: rr.direct,
            last_activity_at: None,
            created_at: b.created_at,
        }
    }
}

const COLUMNS: &str = "account, tab, key, repo, number, title, url, author, is_draft, reasons, team, is_direct, last_activity_at, created_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<(PrRow, String)> {
    let tab = match row.get::<_, String>(1)?.as_str() {
        "mine" => Tab::Mine,
        "review" => Tab::Review,
        other => {
            return Err(rusqlite::Error::FromSqlConversionFailure(
                1,
                rusqlite::types::Type::Text,
                format!("unknown tab {other:?}").into(),
            ));
        }
    };
    let reasons: String = row.get(9)?;
    Ok((
        PrRow {
            account: row.get(0)?,
            tab,
            key: row.get(2)?,
            repo: row.get(3)?,
            number: row.get::<_, i64>(4)? as u64,
            title: row.get(5)?,
            url: row.get(6)?,
            author: row.get(7)?,
            is_draft: row.get(8)?,
            reasons: vec![],
            team: row.get(10)?,
            is_direct: row.get(11)?,
            last_activity_at: row.get(12)?,
            created_at: row.get(13)?,
        },
        reasons,
    ))
}

fn with_reasons((mut row, reasons): (PrRow, String)) -> Result<PrRow> {
    row.reasons = serde_json::from_str(&reasons).map_err(|e| anyhow!("bad reasons for {}: {e}", row.key))?;
    Ok(row)
}

impl Db<'_> {
    pub fn replace_account_prs(&self, account: &str, rows: &[PrRow]) -> Result<()> {
        self.conn
            .execute("DELETE FROM prs WHERE account = ?1", params![account])?;
        let mut insert = self.conn.prepare(&format!(
            "INSERT INTO prs ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)"
        ))?;
        for r in rows {
            insert.execute(params![
                r.account,
                r.tab.as_str(),
                r.key,
                r.repo,
                r.number as i64,
                r.title,
                r.url,
                r.author,
                r.is_draft,
                serde_json::to_string(&r.reasons)?,
                r.team,
                r.is_direct,
                r.last_activity_at,
                r.created_at,
            ])?;
        }
        Ok(())
    }

    pub fn retain_accounts(&self, accounts: &[&str]) -> Result<usize> {
        let mut stmt = self.conn.prepare("SELECT DISTINCT account FROM prs")?;
        let existing: Vec<String> = stmt.query_map([], |row| row.get(0))?.collect::<rusqlite::Result<_>>()?;
        let mut removed = 0;
        for account in existing.iter().filter(|a| !accounts.contains(&a.as_str())) {
            removed += self
                .conn
                .execute("DELETE FROM prs WHERE account = ?1", params![account])?;
        }
        Ok(removed)
    }

    pub fn prs(&self, tab: Tab) -> Result<Vec<PrRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM prs WHERE tab = ?1 ORDER BY key, account"
        ))?;
        let rows = stmt.query_map(params![tab.as_str()], from_row)?;
        Ok(rows
            .filter_map(|r| match r.map_err(anyhow::Error::from).and_then(with_reasons) {
                Ok(row) => Some(row),
                Err(err) => {
                    warn!("skipping unreadable PR row: {err:#}");
                    None
                }
            })
            .collect())
    }

    pub fn prs_for(&self, tab: Tab, key: &str) -> Result<Vec<PrRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM prs WHERE tab = ?1 AND key = ?2 ORDER BY account"
        ))?;
        let rows = stmt.query_map(params![tab.as_str(), key], from_row)?;
        Ok(rows
            .filter_map(|r| match r.map_err(anyhow::Error::from).and_then(with_reasons) {
                Ok(row) => Some(row),
                Err(err) => {
                    warn!("skipping unreadable PR row {key}: {err:#}");
                    None
                }
            })
            .collect())
    }

    pub fn pr(&self, tab: Tab, key: &str) -> Result<Option<PrRow>> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {COLUMNS} FROM prs WHERE tab = ?1 AND key = ?2 ORDER BY account LIMIT 1"
        ))?;
        let mut rows = stmt.query_map(params![tab.as_str(), key], from_row)?;
        match rows.next() {
            None => Ok(None),
            Some(r) => match r.map_err(anyhow::Error::from).and_then(with_reasons) {
                Ok(row) => Ok(Some(row)),
                Err(err) => {
                    warn!("skipping unreadable PR row {key}: {err:#}");
                    Ok(None)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::testkit::*;
    use crate::store::Store;

    fn row(account: &str, tab: Tab, repo: &str, number: u64) -> PrRow {
        let mut r = PrRow::mine(
            account,
            &my_pr(repo, number),
            vec![Reason::Threads { count: 2 }],
            Some(t(1)),
        );
        r.tab = tab;
        r
    }

    #[test]
    fn replace_only_touches_that_account() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        db.replace_account_prs("work", &[row("work", Tab::Mine, "acme/api", 1)])
            .unwrap();
        db.replace_account_prs("home", &[row("home", Tab::Review, "oss/lib", 2)])
            .unwrap();
        db.replace_account_prs("work", &[row("work", Tab::Mine, "acme/api", 3)])
            .unwrap();
        let mine: Vec<String> = db.prs(Tab::Mine).unwrap().into_iter().map(|r| r.key).collect();
        assert_eq!(mine, vec!["acme/api#3"]);
        assert_eq!(db.prs(Tab::Review).unwrap().len(), 1);
    }

    #[test]
    fn round_trips_all_fields() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        let original = row("work", Tab::Mine, "acme/api", 1);
        db.replace_account_prs("work", std::slice::from_ref(&original)).unwrap();
        assert_eq!(db.pr(Tab::Mine, "acme/api#1").unwrap(), Some(original));
        assert_eq!(db.pr(Tab::Review, "acme/api#1").unwrap(), None);
    }

    #[test]
    fn retain_accounts_drops_rows_of_unconfigured_accounts() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        db.replace_account_prs("work", &[row("work", Tab::Mine, "acme/api", 1)])
            .unwrap();
        db.replace_account_prs("gone", &[row("gone", Tab::Review, "oss/lib", 2)])
            .unwrap();
        assert_eq!(db.retain_accounts(&["work"]).unwrap(), 1);
        assert_eq!(db.prs(Tab::Review).unwrap().len(), 0);
        assert_eq!(db.prs(Tab::Mine).unwrap().len(), 1);
    }

    #[test]
    fn review_rows_carry_direct_and_team() {
        let request = crate::domain::model::ReviewRequest {
            base: base("acme/web", 5),
            direct: false,
            team: Some("fe".into()),
            event: None,
            reviews: vec![],
        };
        let row = PrRow::review("work", &request);
        assert_eq!(
            (row.tab, row.key.as_str(), row.is_direct, row.team.as_deref()),
            (Tab::Review, "acme/web#5", false, Some("fe"))
        );
        assert!(row.reasons.is_empty());
    }

    #[test]
    fn unreadable_rows_are_skipped() {
        let store = Store::open_in_memory().unwrap();
        let db = store.db();
        db.replace_account_prs("work", &[row("work", Tab::Mine, "acme/api", 1)])
            .unwrap();
        db.conn
            .execute(
                "INSERT INTO prs (account, tab, key, repo, number, title, url, author, is_draft, reasons, is_direct, created_at)
                 VALUES ('work', 'mine', 'acme/bad#2', 'acme/bad', 2, 't', 'u', 'a', 0, 'not json', 1, 'x')",
                [],
            )
            .unwrap();
        let keys: Vec<String> = db.prs(Tab::Mine).unwrap().into_iter().map(|r| r.key).collect();
        assert_eq!(keys, vec!["acme/api#1"]);
        assert_eq!(db.pr(Tab::Mine, "acme/bad#2").unwrap(), None);
    }
}
