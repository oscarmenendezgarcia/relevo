PRAGMA foreign_keys=OFF;
BEGIN TRANSACTION;
CREATE TABLE people (
    id         INTEGER PRIMARY KEY,
    handle     TEXT NOT NULL UNIQUE,
    name       TEXT NOT NULL,
    about      TEXT NOT NULL DEFAULT '',
    active     INTEGER NOT NULL DEFAULT 1,
    created_at INTEGER NOT NULL
);
INSERT INTO people VALUES(1,'relevo','Relevo','the release notes of each version',0,1794770648);
INSERT INTO people VALUES(2,'ana','Ana','payments backend',1,1794770648);
INSERT INTO people VALUES(3,'bob','Bob','infrastructure',1,1794770648);
INSERT INTO people VALUES(4,'carl','Carl','billing',1,1794770648);
CREATE TABLE tokens (
    id         TEXT PRIMARY KEY,
    person_id  INTEGER NOT NULL REFERENCES people(id),
    label      TEXT NOT NULL,
    hash       TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER
);
INSERT INTO tokens VALUES('t_mwju5ne7ondt',1,'release notes','system:t_mwju5ne7ondt',1794770648,1794770648);
INSERT INTO tokens VALUES('t_x4jwhzkgxn76',2,'Claude Code','53bee2fe316bde0ba15838ff96be1a669017250e3cc483b7022b17c15a606ed0',1794770648,NULL);
INSERT INTO tokens VALUES('t_4mjylzsqlo2w',3,'pi','5a7101036c10bd2a33240f0bdb4da6ce92cbfc07bb2010cb8badb14a65ece802',1794770648,NULL);
INSERT INTO tokens VALUES('t_owag3lnf26ft',4,'Hermes','c4d39a7036c53174ac12f05cf49f617390ad388bd3567123e1ee069e7ea26347',1794770648,NULL);
CREATE TABLE threads (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('direct', 'team', 'global')),
    title      TEXT NOT NULL,
    opener_id  INTEGER NOT NULL REFERENCES people(id),
    status     TEXT NOT NULL CHECK (status IN ('open', 'closed')),
    created_at INTEGER NOT NULL
);
INSERT INTO threads VALUES('r_v2n6unnwxcwv','direct','@bob Fetched, not answered',2,'open',1794770649);
INSERT INTO threads VALUES('r_5habdossjz5p','direct','@bob Deploy tonight?',4,'open',1794770649);
INSERT INTO threads VALUES('r_5wxwobu6732c','team','@backend Is staging down?',2,'open',1794770649);
INSERT INTO threads VALUES('r_cnhx4nnlozfj','direct','@bob Never fetched.',2,'open',1794770649);
CREATE TABLE sessions (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id        TEXT NOT NULL REFERENCES tokens(id),
    client_id       TEXT NOT NULL,
    person_id       INTEGER NOT NULL REFERENCES people(id),
    context         TEXT,
    last_seen       INTEGER NOT NULL,
    claims          INTEGER NOT NULL DEFAULT 0,
    client          TEXT,
    client_inferred INTEGER NOT NULL DEFAULT 0,
    UNIQUE (token_id, client_id)
);
INSERT INTO sessions VALUES(1,'t_x4jwhzkgxn76','ana-cc-0001',2,'pagos-api',1794770649,0,'mcp/0.5.0',0);
INSERT INTO sessions VALUES(2,'t_4mjylzsqlo2w','bob-term-01',3,'pagos-api',1794770649,1,'pi/0.5.0',0);
INSERT INTO sessions VALUES(3,'t_owag3lnf26ft','carl-hermes1',4,NULL,1794770649,0,'hermes/0.5.0',0);
CREATE TABLE participants (
    thread_id         TEXT NOT NULL REFERENCES threads(id),
    person_id         INTEGER NOT NULL REFERENCES people(id),
    context           TEXT,
    claimed_by        INTEGER REFERENCES sessions(id),
    claimed_context   TEXT,
    claimed_at        INTEGER,
    delivered_seq     INTEGER,
    delivered_at      INTEGER,
    delivered_label   TEXT,
    delivered_context TEXT,
    last_seq          INTEGER NOT NULL DEFAULT 0,
    last_other_seq    INTEGER NOT NULL DEFAULT 0,
    has_new           INTEGER NOT NULL DEFAULT 0,
    waiting           INTEGER NOT NULL DEFAULT 0,
    inbox             INTEGER NOT NULL DEFAULT 0,
    resolved          INTEGER NOT NULL DEFAULT 0,
    handled_seq       INTEGER,
    handled_at        INTEGER,
    handled_label     TEXT,
    handled_context   TEXT,
    fresh             INTEGER NOT NULL DEFAULT 0,
    named_seq         INTEGER,
    PRIMARY KEY (thread_id, person_id)
);
INSERT INTO participants VALUES('r_v2n6unnwxcwv',2,NULL,NULL,NULL,NULL,1,NULL,NULL,NULL,1,0,0,0,0,0,1,NULL,NULL,NULL,0,0);
INSERT INTO participants VALUES('r_v2n6unnwxcwv',3,NULL,NULL,NULL,NULL,1,1794770649,'pi','pagos-api',1,1,0,1,1,0,NULL,NULL,NULL,NULL,1,0);
INSERT INTO participants VALUES('r_5habdossjz5p',4,NULL,NULL,NULL,NULL,2,NULL,NULL,NULL,3,3,1,1,1,0,2,NULL,NULL,NULL,0,0);
INSERT INTO participants VALUES('r_5habdossjz5p',3,NULL,2,'pagos-api',1794770649,3,1794770649,'pi','pagos-api',3,2,0,0,0,0,3,1794770649,'pi','pagos-api',0,0);
INSERT INTO participants VALUES('r_5wxwobu6732c',2,NULL,NULL,NULL,NULL,4,NULL,NULL,NULL,4,0,0,0,0,0,4,NULL,NULL,NULL,0,NULL);
INSERT INTO participants VALUES('r_5wxwobu6732c',3,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,4,4,1,0,1,0,NULL,NULL,NULL,NULL,0,NULL);
INSERT INTO participants VALUES('r_5wxwobu6732c',4,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,4,4,1,0,1,0,NULL,NULL,NULL,NULL,0,NULL);
INSERT INTO participants VALUES('r_cnhx4nnlozfj',2,NULL,NULL,NULL,NULL,5,NULL,NULL,NULL,5,0,0,0,0,0,5,NULL,NULL,NULL,0,0);
INSERT INTO participants VALUES('r_cnhx4nnlozfj',3,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,5,5,1,1,1,0,NULL,NULL,NULL,NULL,0,0);
CREATE TABLE session_threads (
    session_id    INTEGER NOT NULL REFERENCES sessions(id),
    thread_id     TEXT NOT NULL REFERENCES threads(id),
    person_id     INTEGER NOT NULL REFERENCES people(id),
    delivered_seq INTEGER NOT NULL,
    has_new       INTEGER NOT NULL DEFAULT 0,
    waiting       INTEGER NOT NULL DEFAULT 0,
    last_seq      INTEGER NOT NULL DEFAULT 0,
    claimed_by    INTEGER,
    context       TEXT,
    person_inbox  INTEGER NOT NULL DEFAULT 0,
    resolved      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (session_id, thread_id)
);
CREATE TABLE notes (
    seq        INTEGER PRIMARY KEY AUTOINCREMENT,
    id         TEXT NOT NULL UNIQUE,
    thread_id  TEXT NOT NULL REFERENCES threads(id),
    author_id  INTEGER NOT NULL REFERENCES people(id),
    token_id   TEXT NOT NULL REFERENCES tokens(id),
    via        TEXT NOT NULL,
    text       TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
INSERT INTO notes VALUES(1,'n_x4sbxrn5zwbj','r_v2n6unnwxcwv',2,'t_x4jwhzkgxn76','Claude Code','@bob Fetched, not answered',1794770649);
INSERT INTO notes VALUES(2,'n_buacq7qzpylm','r_5habdossjz5p',4,'t_owag3lnf26ft','Hermes','@bob Deploy tonight?',1794770649);
INSERT INTO notes VALUES(3,'n_z5tnyqrbixl6','r_5habdossjz5p',3,'t_4mjylzsqlo2w','pi','Yes, at nine.',1794770649);
INSERT INTO notes VALUES(4,'n_74fi2s4c4nny','r_5wxwobu6732c',2,'t_x4jwhzkgxn76','Claude Code','@backend Is staging down?',1794770649);
INSERT INTO notes VALUES(5,'n_txrsaaamxy7k','r_cnhx4nnlozfj',2,'t_x4jwhzkgxn76','Claude Code','@bob Never fetched.',1794770649);
CREATE TABLE send_keys (
    person_id  INTEGER NOT NULL REFERENCES people(id),
    key        TEXT NOT NULL,
    thread_id  TEXT NOT NULL,
    note_id    TEXT,
    status     TEXT NOT NULL,
    claimed_by INTEGER,
    created_at INTEGER NOT NULL,
    result     TEXT,
    PRIMARY KEY (person_id, key)
);
INSERT INTO send_keys VALUES(2,'k-fetched','r_v2n6unnwxcwv','n_x4sbxrn5zwbj','open',NULL,1794770649,'{"kind":"direct","to":{"people":["bob"],"teams":[]},"added":[],"reached":1}');
CREATE TABLE teams (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    title      TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
);
INSERT INTO teams VALUES(1,'backend','Backend',1794770648);
CREATE TABLE team_members (
    team_id   INTEGER NOT NULL REFERENCES teams(id),
    person_id INTEGER NOT NULL REFERENCES people(id),
    PRIMARY KEY (team_id, person_id)
);
INSERT INTO team_members VALUES(1,3);
INSERT INTO team_members VALUES(1,4);
CREATE TABLE thread_teams (
    thread_id TEXT NOT NULL REFERENCES threads(id),
    name      TEXT NOT NULL,
    PRIMARY KEY (thread_id, name)
);
INSERT INTO thread_teams VALUES('r_5wxwobu6732c','backend');
CREATE TABLE note_contexts (
    seq     INTEGER PRIMARY KEY REFERENCES notes(seq),
    context TEXT NOT NULL
);
INSERT INTO note_contexts VALUES(1,'pagos-api');
INSERT INTO note_contexts VALUES(3,'pagos-api');
INSERT INTO note_contexts VALUES(4,'pagos-api');
INSERT INTO note_contexts VALUES(5,'pagos-api');
CREATE TABLE thread_closes (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id),
    person_id INTEGER NOT NULL REFERENCES people(id),
    closed_at INTEGER NOT NULL
);
CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO meta VALUES('system_person','1');
INSERT INTO meta VALUES('announced','0.5.0');
DELETE FROM sqlite_sequence;
INSERT INTO sqlite_sequence VALUES('sessions',10);
INSERT INTO sqlite_sequence VALUES('notes',5);
CREATE INDEX sessions_by_person ON sessions(person_id, last_seen);
CREATE INDEX sessions_by_last_seen ON sessions(last_seen);
CREATE INDEX sessions_claiming ON sessions(last_seen) WHERE claims = 1;
CREATE INDEX threads_by_opener ON threads(opener_id, kind, created_at);
CREATE INDEX session_threads_by_thread ON session_threads(thread_id);
CREATE INDEX session_threads_by_claim
    ON session_threads(claimed_by) WHERE claimed_by IS NOT NULL;
CREATE INDEX participants_by_person ON participants(person_id);
CREATE INDEX participants_fresh ON participants(person_id) WHERE fresh = 1;
CREATE INDEX participants_by_claim
    ON participants(claimed_by) WHERE claimed_by IS NOT NULL;
CREATE INDEX participants_inbox
    ON participants(person_id, has_new, waiting, last_seq) WHERE inbox = 1;
CREATE INDEX participants_inbox_claim
    ON participants(person_id, claimed_by, has_new, waiting, last_seq) WHERE inbox = 1;
CREATE INDEX participants_inbox_context
    ON participants(person_id, claimed_by, context, has_new, waiting, last_seq) WHERE inbox = 1;
CREATE INDEX session_new
    ON session_threads(session_id, waiting, last_seq) WHERE has_new = 1;
CREATE INDEX session_new_claim
    ON session_threads(session_id, claimed_by, waiting, last_seq) WHERE has_new = 1;
CREATE INDEX session_new_context
    ON session_threads(session_id, claimed_by, context, waiting, last_seq) WHERE has_new = 1;
CREATE INDEX session_count
    ON session_threads(session_id, claimed_by, context)
    WHERE has_new = 1 AND person_inbox = 0;
CREATE INDEX notes_by_thread ON notes(thread_id, seq);
CREATE INDEX notes_by_author ON notes(author_id, created_at);
CREATE INDEX send_keys_by_time ON send_keys(created_at);
CREATE INDEX team_members_by_person ON team_members(person_id);
CREATE INDEX participants_timeline ON participants(person_id, last_seq);
CREATE INDEX notes_by_thread_author ON notes(thread_id, author_id);
COMMIT;
PRAGMA user_version=9;
