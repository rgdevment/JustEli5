//! `memory.sqlite`: unlike the corpus index, this database is the source of truth for what is
//! known between meetings, so it is never rebuilt; `export` is its backup.

use std::path::Path;
use std::time::{Duration, Instant};

use mc_core::fold::fold;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize};

use crate::error::MemoryError;
use crate::model::{Concept, Form, Kind, Role, Sighting, State};
use crate::resolve::{Candidate, Context, Resolution, decide};

/// Entry `i` upgrades the schema from version `i` to `i + 1`.
const MIGRATIONS: &[&str] = &[
    r"
CREATE TABLE concepts (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind IN ('term', 'person')),
    name TEXT NOT NULL,
    meaning TEXT,
    state TEXT NOT NULL CHECK (state IN ('proposed', 'confirmed')),
    updated_ms INTEGER NOT NULL
);
CREATE TABLE forms (
    concept INTEGER NOT NULL REFERENCES concepts(id) ON DELETE CASCADE,
    text TEXT NOT NULL,
    key TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('proposed', 'confirmed')),
    PRIMARY KEY (concept, key)
);
CREATE INDEX forms_by_key ON forms(key);
CREATE TABLE roles (
    person INTEGER NOT NULL REFERENCES concepts(id) ON DELETE CASCADE,
    role TEXT NOT NULL,
    since_ms INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('proposed', 'confirmed'))
);
CREATE INDEX roles_by_person ON roles(person);
CREATE TABLE sightings (
    concept INTEGER NOT NULL REFERENCES concepts(id) ON DELETE CASCADE,
    session TEXT NOT NULL,
    series TEXT,
    at_ms INTEGER NOT NULL,
    attended INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (concept, session)
);
CREATE INDEX sightings_by_session ON sightings(session);
",
    r"
CREATE TABLE adopted (
    source TEXT PRIMARY KEY,
    at_ms INTEGER NOT NULL
);
",
];

const EXPORT_FORMAT: &str = "justeli5.memory/1";
const BUSY: Duration = Duration::from_secs(5);

/// SQLite does not wait on a busy database to change its journal mode, and several commands
/// open a new memory at once: the switch is retried until the wait would be over.
fn to_wal(conn: &Connection) -> Result<(), MemoryError> {
    let until = Instant::now() + BUSY;
    loop {
        match conn
            .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get::<_, String>(0))
        {
            Ok(_) => return Ok(()),
            Err(rusqlite::Error::SqliteFailure(e, _))
                if e.code == rusqlite::ErrorCode::DatabaseBusy && Instant::now() < until =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(e.into()),
        }
    }
}

pub struct Memory {
    conn: Connection,
}

/// An entry kept elsewhere before the memory existed, taken in once by [`Memory::adopt`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Adopted {
    pub kind: Kind,
    pub name: String,
    pub meaning: Option<String>,
    pub forms: Vec<String>,
    pub role: Option<String>,
}

/// One concept with everything hanging from it, as `export` writes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Exported {
    kind: Kind,
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    meaning: Option<String>,
    state: State,
    forms: Vec<Form>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    roles: Vec<Role>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    sightings: Vec<Sighting>,
}

#[derive(Serialize, Deserialize)]
struct Export {
    format: String,
    concepts: Vec<Exported>,
}

/// The key a form is looked up by, blind to case, accents and spacing: Whisper splits and joins
/// words, so "Beri dex", "beri-dex" and "Beridex" are one form.
fn key_of(text: &str) -> Result<String, MemoryError> {
    let key = fold(text);
    if key.is_empty() {
        return Err(MemoryError::Invalid(format!(
            "{text:?} has no letters or digits"
        )));
    }
    Ok(key)
}

fn tidy(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn state_of(text: &str) -> Result<State, MemoryError> {
    State::parse(text).ok_or_else(|| MemoryError::Invalid(format!("unknown state {text:?}")))
}

impl Memory {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, MemoryError> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY)?;
        // The capture writes a correction while the window reads the list.
        to_wal(&conn)?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self, MemoryError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self, MemoryError> {
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(BUSY)?;
        let mut memory = Self { conn };
        memory.migrate()?;
        Ok(memory)
    }

    fn migrate(&mut self) -> Result<(), MemoryError> {
        let known = MIGRATIONS.len() as i64;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let found: i64 = tx.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if found > known {
            return Err(MemoryError::SchemaTooNew { found, known });
        }
        for (index, sql) in MIGRATIONS
            .iter()
            .enumerate()
            .skip(usize::try_from(found).unwrap_or(0))
        {
            tx.execute_batch(sql)?;
            tx.pragma_update(None, "user_version", index as i64 + 1)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// A new concept named `name`, which is also its first form. A name another concept already
    /// has is allowed: that is a homonym.
    pub fn add(
        &mut self,
        kind: Kind,
        name: &str,
        meaning: Option<&str>,
        state: State,
        at_ms: i64,
    ) -> Result<i64, MemoryError> {
        let name = tidy(name);
        key_of(&name)?;
        let meaning = meaning.map(tidy).filter(|m| !m.is_empty());
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO concepts (kind, name, meaning, state, updated_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![kind.as_str(), name, meaning, state.as_str(), at_ms],
        )?;
        let id = tx.last_insert_rowid();
        insert_form(&tx, id, &name, state)?;
        tx.commit()?;
        Ok(id)
    }

    pub fn get(&self, id: i64) -> Result<Concept, MemoryError> {
        let row = self
            .conn
            .query_row(
                "SELECT kind, name, meaning, state FROM concepts WHERE id = ?1",
                [id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        let (kind, name, meaning, state) = row.ok_or(MemoryError::Missing(id))?;
        let kind = Kind::parse(&kind)
            .ok_or_else(|| MemoryError::Invalid(format!("unknown kind {kind:?}")))?;
        let mut forms = self.conn.prepare_cached(
            "SELECT text, state FROM forms WHERE concept = ?1 ORDER BY text = ?2 DESC, text",
        )?;
        let forms = forms
            .query_map(params![id, name], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .map(|r| {
                let (text, state) = r?;
                Ok(Form {
                    text,
                    state: state_of(&state)?,
                })
            })
            .collect::<Result<Vec<_>, MemoryError>>()?;
        let mut roles = self.conn.prepare_cached(
            "SELECT role, since_ms, state FROM roles WHERE person = ?1 ORDER BY since_ms DESC, rowid DESC",
        )?;
        let roles = roles
            .query_map([id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .map(|r| {
                let (role, since_ms, state) = r?;
                Ok(Role {
                    role,
                    since_ms,
                    state: state_of(&state)?,
                })
            })
            .collect::<Result<Vec<_>, MemoryError>>()?;
        let meetings: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM sightings WHERE concept = ?1",
            [id],
            |row| row.get(0),
        )?;
        Ok(Concept {
            id,
            kind,
            name,
            meaning,
            state: state_of(&state)?,
            forms,
            roles,
            meetings: usize::try_from(meetings).unwrap_or(0),
        })
    }

    /// Concepts whose forms or meaning contain `query` (any case, any accents), the most seen first.
    pub fn list(
        &self,
        kind: Option<Kind>,
        query: Option<&str>,
        state: Option<State>,
    ) -> Result<Vec<Concept>, MemoryError> {
        let needle = query.map(fold).unwrap_or_default();
        let mut ids = self.conn.prepare_cached(
            "SELECT c.id, c.meaning FROM concepts c
             WHERE (?1 IS NULL OR c.kind = ?1) AND (?2 IS NULL OR c.state = ?2)
             ORDER BY (SELECT COUNT(*) FROM sightings s WHERE s.concept = c.id) DESC, c.name",
        )?;
        let rows = ids
            .query_map(
                params![kind.map(Kind::as_str), state.map(State::as_str)],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let mut found = Vec::new();
        for (id, meaning) in rows {
            let concept = self.get(id)?;
            let hit = needle.is_empty()
                || concept
                    .forms
                    .iter()
                    .any(|f| fold(&f.text).contains(&needle))
                || meaning.is_some_and(|m| fold(&m).contains(&needle));
            if hit {
                found.push(concept);
            }
        }
        Ok(found)
    }

    /// Adds a form to a concept, or confirms one it has; a confirmed form stays confirmed.
    pub fn add_form(&mut self, id: i64, text: &str, state: State) -> Result<(), MemoryError> {
        self.get(id)?;
        let tx = self.conn.transaction()?;
        insert_form(&tx, id, &tidy(text), state)?;
        tx.commit()?;
        Ok(())
    }

    /// Takes a form off a concept; its name stays, since the name is how it is shown.
    pub fn remove_form(&mut self, id: i64, text: &str) -> Result<(), MemoryError> {
        let concept = self.get(id)?;
        let key = key_of(text)?;
        if fold(&concept.name) == key {
            return Err(MemoryError::Invalid(format!(
                "{text:?} is the name of the entry; rename it first"
            )));
        }
        self.conn.execute(
            "DELETE FROM forms WHERE concept = ?1 AND key = ?2",
            params![id, key],
        )?;
        Ok(())
    }

    /// The user says `text` is a form of `id`: an entry of the same kind named `text` was this
    /// one all along and joins it, unless both say what they are and say different things (then
    /// they are homonyms). Returns the entries that joined.
    pub fn claim_form(&mut self, id: i64, text: &str, at_ms: i64) -> Result<Vec<i64>, MemoryError> {
        let owner = self.get(id)?;
        self.add_form(id, text, State::Confirmed)?;
        let key = key_of(text)?;
        let mut joined = Vec::new();
        for other in self.candidates(owner.kind, text)? {
            if other == id {
                continue;
            }
            let entry = self.get(other)?;
            let same = match (&entry.meaning, &owner.meaning) {
                (Some(a), Some(b)) => fold(a) == fold(b),
                _ => true,
            };
            if fold(&entry.name) == key && same {
                self.merge(id, other, at_ms)?;
                joined.push(other);
            }
        }
        Ok(joined)
    }

    /// Entries left loose although another entry of their kind claims their name as a form, as
    /// before `claim_form` existed: they join it on the same terms. Returns the entries that joined.
    pub fn tidy(&mut self, at_ms: i64) -> Result<Vec<i64>, MemoryError> {
        let mut joined = Vec::new();
        for owner in self.list(None, None, None)? {
            if joined.contains(&owner.id) {
                continue;
            }
            for form in owner
                .forms
                .iter()
                .filter(|f| fold(&f.text) != fold(&owner.name))
            {
                joined.extend(self.claim_form(owner.id, &form.text, at_ms)?);
            }
        }
        Ok(joined)
    }

    /// The form is an entry of its own after all: it leaves `id` and stands apart, confirmed.
    pub fn split_form(&mut self, id: i64, text: &str, at_ms: i64) -> Result<i64, MemoryError> {
        let kind = self.get(id)?.kind;
        self.remove_form(id, text)?;
        self.add(kind, text, None, State::Confirmed, at_ms)
    }

    /// The form belongs to the other concept: "Order" here is the team, not a purchase order.
    pub fn move_form(&mut self, from: i64, to: i64, text: &str) -> Result<(), MemoryError> {
        let (source, target) = (self.get(from)?, self.get(to)?);
        if source.kind != target.kind {
            return Err(MemoryError::Invalid(
                "a form moves only between two terms or two people".to_string(),
            ));
        }
        self.remove_form(from, text)?;
        self.add_form(to, text, State::Confirmed)
    }

    /// A new canonical form; the old name stays as one more form.
    pub fn rename(&mut self, id: i64, name: &str, at_ms: i64) -> Result<(), MemoryError> {
        self.get(id)?;
        let name = tidy(name);
        let tx = self.conn.transaction()?;
        insert_form(&tx, id, &name, State::Confirmed)?;
        tx.execute(
            "UPDATE concepts SET name = ?2, updated_ms = ?3 WHERE id = ?1",
            params![id, name, at_ms],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn set_meaning(
        &mut self,
        id: i64,
        meaning: Option<&str>,
        at_ms: i64,
    ) -> Result<(), MemoryError> {
        self.get(id)?;
        let meaning = meaning.map(tidy).filter(|m| !m.is_empty());
        self.conn.execute(
            "UPDATE concepts SET meaning = ?2, updated_ms = ?3 WHERE id = ?1",
            params![id, meaning, at_ms],
        )?;
        Ok(())
    }

    pub fn set_role(
        &mut self,
        person: i64,
        role: &str,
        since_ms: i64,
        state: State,
    ) -> Result<(), MemoryError> {
        if self.get(person)?.kind != Kind::Person {
            return Err(MemoryError::Invalid("only people have a role".to_string()));
        }
        let role = tidy(role);
        if role.is_empty() {
            return Err(MemoryError::Invalid("the role is empty".to_string()));
        }
        self.conn.execute(
            "INSERT INTO roles (person, role, since_ms, state) VALUES (?1, ?2, ?3, ?4)",
            params![person, role, since_ms, state.as_str()],
        )?;
        Ok(())
    }

    /// The user vouches for the concept as it stands: its name, forms and newest role.
    pub fn confirm(&mut self, id: i64, at_ms: i64) -> Result<(), MemoryError> {
        self.get(id)?;
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE concepts SET state = 'confirmed', updated_ms = ?2 WHERE id = ?1",
            params![id, at_ms],
        )?;
        tx.execute(
            "UPDATE forms SET state = 'confirmed' WHERE concept = ?1",
            [id],
        )?;
        tx.execute(
            "UPDATE roles SET state = 'confirmed' WHERE rowid = (
                 SELECT rowid FROM roles WHERE person = ?1 ORDER BY since_ms DESC, rowid DESC LIMIT 1)",
            [id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn remove(&mut self, id: i64) -> Result<(), MemoryError> {
        self.get(id)?;
        self.conn
            .execute("DELETE FROM concepts WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Two entries were one: `gone` hands its forms, roles and meetings to `keep` and disappears.
    pub fn merge(&mut self, keep: i64, gone: i64, at_ms: i64) -> Result<(), MemoryError> {
        let (kept, merged) = (self.get(keep)?, self.get(gone)?);
        if keep == gone || kept.kind != merged.kind {
            return Err(MemoryError::Invalid(
                "only two different terms or two different people merge".to_string(),
            ));
        }
        let tx = self.conn.transaction()?;
        for form in &merged.forms {
            insert_form(&tx, keep, &form.text, form.state)?;
        }
        tx.execute(
            "UPDATE roles SET person = ?1 WHERE person = ?2",
            params![keep, gone],
        )?;
        tx.execute(
            "INSERT OR IGNORE INTO sightings (concept, session, series, at_ms, attended)
             SELECT ?1, session, series, at_ms, attended FROM sightings WHERE concept = ?2",
            params![keep, gone],
        )?;
        if kept.meaning.is_none() {
            tx.execute(
                "UPDATE concepts SET meaning = ?2 WHERE id = ?1",
                params![keep, merged.meaning],
            )?;
        }
        tx.execute(
            "UPDATE concepts SET state = 'confirmed', updated_ms = ?2 WHERE id = ?1",
            params![keep, at_ms],
        )?;
        tx.execute("DELETE FROM concepts WHERE id = ?1", [gone])?;
        tx.commit()?;
        Ok(())
    }

    /// The concept came up in a meeting; a second sighting in the same meeting replaces the first.
    pub fn record(&mut self, id: i64, sighting: &Sighting) -> Result<(), MemoryError> {
        self.get(id)?;
        self.conn.execute(
            "INSERT INTO sightings (concept, session, series, at_ms, attended)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (concept, session) DO UPDATE SET
                 series = excluded.series, at_ms = excluded.at_ms,
                 attended = MAX(sightings.attended, excluded.attended)",
            params![
                id,
                sighting.session,
                sighting.series,
                sighting.at_ms,
                sighting.attended
            ],
        )?;
        Ok(())
    }

    /// The meetings a concept came up in, the newest first.
    pub fn sightings(&self, id: i64) -> Result<Vec<Sighting>, MemoryError> {
        let mut stmt = self.conn.prepare_cached(
            "SELECT session, series, at_ms, attended FROM sightings WHERE concept = ?1 ORDER BY at_ms DESC",
        )?;
        let rows = stmt
            .query_map([id], |row| {
                Ok(Sighting {
                    session: row.get(0)?,
                    series: row.get(1)?,
                    at_ms: row.get(2)?,
                    attended: row.get(3)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// The concepts of `kind` that `text` is a form of.
    pub fn candidates(&self, kind: Kind, text: &str) -> Result<Vec<i64>, MemoryError> {
        let key = key_of(text)?;
        let mut stmt = self.conn.prepare_cached(
            "SELECT DISTINCT f.concept FROM forms f JOIN concepts c ON c.id = f.concept
             WHERE f.key = ?1 AND c.kind = ?2 ORDER BY f.concept",
        )?;
        let ids = stmt
            .query_map(params![key, kind.as_str()], |row| row.get(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    /// Which concept `text` means in this meeting, if the memory can tell.
    pub fn resolve(
        &self,
        kind: Kind,
        text: &str,
        context: &Context,
    ) -> Result<Resolution, MemoryError> {
        let mut scored = Vec::new();
        for id in self.candidates(kind, text)? {
            scored.push(Candidate {
                id,
                score: self.score(id, context)?,
            });
        }
        Ok(decide(scored))
    }

    fn score(&self, id: i64, context: &Context) -> Result<u32, MemoryError> {
        let in_series: i64 = match &context.series {
            Some(series) => self.conn.query_row(
                "SELECT COUNT(*) FROM sightings WHERE concept = ?1 AND series = ?2",
                params![id, series],
                |row| row.get(0),
            )?,
            None => 0,
        };
        let present: Vec<i64> = context
            .present
            .iter()
            .copied()
            .filter(|&p| p != id)
            .collect();
        let shared: i64 = if present.is_empty() {
            0
        } else {
            let marks = vec!["?"; present.len()].join(", ");
            let sql = format!(
                "SELECT COUNT(DISTINCT mine.session) FROM sightings mine
                 JOIN sightings other ON other.session = mine.session
                 WHERE mine.concept = ? AND other.concept IN ({marks})"
            );
            let mut values = vec![id];
            values.extend(present);
            self.conn
                .query_row(&sql, rusqlite::params_from_iter(values), |row| row.get(0))?
        };
        Ok(u32::try_from(in_series * 2 + shared).unwrap_or(u32::MAX))
    }

    /// Takes in what `source` held, confirmed, the first time only: two commands opening a new
    /// memory at once must not both copy it. Returns whether this call did.
    pub fn adopt(
        &mut self,
        source: &str,
        entries: &[Adopted],
        at_ms: i64,
    ) -> Result<bool, MemoryError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let done: Option<i64> = tx
            .query_row(
                "SELECT at_ms FROM adopted WHERE source = ?1",
                [source],
                |row| row.get(0),
            )
            .optional()?;
        if done.is_some() {
            return Ok(false);
        }
        for entry in entries {
            let name = tidy(&entry.name);
            if fold(&name).is_empty() {
                continue;
            }
            tx.execute(
                "INSERT INTO concepts (kind, name, meaning, state, updated_ms) VALUES (?1, ?2, ?3, 'confirmed', ?4)",
                params![entry.kind.as_str(), name, entry.meaning, at_ms],
            )?;
            let id = tx.last_insert_rowid();
            insert_form(&tx, id, &name, State::Confirmed)?;
            for form in entry.forms.iter().filter(|f| !fold(f).is_empty()) {
                insert_form(&tx, id, &tidy(form), State::Confirmed)?;
            }
            if let Some(role) = entry.role.as_deref().map(tidy).filter(|r| !r.is_empty()) {
                tx.execute(
                    "INSERT INTO roles (person, role, since_ms, state) VALUES (?1, ?2, ?3, 'confirmed')",
                    params![id, role, at_ms],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO adopted (source, at_ms) VALUES (?1, ?2)",
            params![source, at_ms],
        )?;
        tx.commit()?;
        Ok(true)
    }

    /// Everything, as JSON a person can read and keep.
    pub fn export(&self) -> Result<String, MemoryError> {
        let mut concepts = Vec::new();
        for concept in self.list(None, None, None)? {
            concepts.push(Exported {
                sightings: self.sightings(concept.id)?,
                kind: concept.kind,
                name: concept.name,
                meaning: concept.meaning,
                state: concept.state,
                forms: concept.forms,
                roles: concept.roles,
            });
        }
        Ok(serde_json::to_string_pretty(&Export {
            format: EXPORT_FORMAT.to_string(),
            concepts,
        })?)
    }

    /// Loads an export into an empty memory; into one that holds anything it would duplicate.
    pub fn import(&mut self, json: &str, at_ms: i64) -> Result<usize, MemoryError> {
        let export: Export = serde_json::from_str(json)?;
        if export.format != EXPORT_FORMAT {
            return Err(MemoryError::Invalid(format!(
                "unknown export format {:?}",
                export.format
            )));
        }
        let held: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM concepts", [], |row| row.get(0))?;
        if held > 0 {
            return Err(MemoryError::Invalid(
                "the memory is not empty; an import would duplicate it".to_string(),
            ));
        }
        let tx = self.conn.transaction()?;
        for concept in &export.concepts {
            let name = tidy(&concept.name);
            key_of(&name)?;
            tx.execute(
                "INSERT INTO concepts (kind, name, meaning, state, updated_ms) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    concept.kind.as_str(),
                    name,
                    concept.meaning,
                    concept.state.as_str(),
                    at_ms
                ],
            )?;
            let id = tx.last_insert_rowid();
            insert_form(&tx, id, &name, concept.state)?;
            for form in &concept.forms {
                insert_form(&tx, id, &form.text, form.state)?;
            }
            for role in &concept.roles {
                tx.execute(
                    "INSERT INTO roles (person, role, since_ms, state) VALUES (?1, ?2, ?3, ?4)",
                    params![id, role.role, role.since_ms, role.state.as_str()],
                )?;
            }
            for s in &concept.sightings {
                tx.execute(
                    "INSERT OR IGNORE INTO sightings (concept, session, series, at_ms, attended)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, s.session, s.series, s.at_ms, s.attended],
                )?;
            }
        }
        tx.commit()?;
        Ok(export.concepts.len())
    }
}

fn insert_form(conn: &Connection, id: i64, text: &str, state: State) -> Result<(), MemoryError> {
    let key = key_of(text)?;
    conn.execute(
        "INSERT INTO forms (concept, text, key, state) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (concept, key) DO UPDATE SET
             state = CASE WHEN excluded.state = 'confirmed' THEN 'confirmed' ELSE forms.state END",
        params![id, text, key, state.as_str()],
    )?;
    Ok(())
}
