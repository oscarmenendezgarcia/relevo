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
INSERT INTO people VALUES(1,'relevo','Relevo','the release notes of each version',0,1794653694);
INSERT INTO people VALUES(2,'ana','Ana','payments backend',1,1794653694);
INSERT INTO people VALUES(3,'bob','Bob','infrastructure',1,1794653694);
INSERT INTO people VALUES(4,'carl','Carl','billing',1,1794653694);
CREATE TABLE tokens (
    id         TEXT PRIMARY KEY,
    person_id  INTEGER NOT NULL REFERENCES people(id),
    label      TEXT NOT NULL,
    hash       TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER
);
INSERT INTO tokens VALUES('t_czfiofdj6pqp',1,'release notes','system:t_czfiofdj6pqp',1794653694,1794653694);
INSERT INTO tokens VALUES('t_ihvylz6kerxm',2,'Claude Code','328fdc2d2aa22994d1c312d96482fbde87ebbd02030b169387916d277e9a8ae0',1794653694,NULL);
INSERT INTO tokens VALUES('t_sqnkiyau35os',3,'pi','4102f194ce015d20c3e3b92ca82e2cfbe956bf78adb7b118948548bb72f6d524',1794653694,NULL);
INSERT INTO tokens VALUES('t_dnoyb7q3s4f7',4,'Hermes','74f85e1a6746c3a7c1c7a0ea57071d2382e9680d90a95a6c28b3079b4226a507',1794653694,NULL);
CREATE TABLE threads (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('direct', 'team', 'global')),
    title      TEXT NOT NULL,
    opener_id  INTEGER NOT NULL REFERENCES people(id),
    status     TEXT NOT NULL CHECK (status IN ('open', 'closed')),
    created_at INTEGER NOT NULL
);
INSERT INTO threads VALUES('r_or5d7sm54l6v','direct','@bob Fetched, not answered',2,'open',1794653694);
INSERT INTO threads VALUES('r_5y2x7chazch5','direct','@bob Deploy tonight?',4,'open',1794653694);
INSERT INTO threads VALUES('r_bga7tgjxwctl','team','@backend Is staging down?',2,'open',1794653694);
INSERT INTO threads VALUES('r_7lt67jckgbsb','direct','@bob Never fetched.',2,'open',1794653694);
CREATE TABLE sessions (
    id         INTEGER PRIMARY KEY AUTOINCREMENT,
    token_id   TEXT NOT NULL REFERENCES tokens(id),
    client_id  TEXT NOT NULL,
    person_id  INTEGER NOT NULL REFERENCES people(id),
    context    TEXT,
    last_seen  INTEGER NOT NULL,
    claims     INTEGER NOT NULL DEFAULT 0,
    UNIQUE (token_id, client_id)
);
INSERT INTO sessions VALUES(1,'t_ihvylz6kerxm','ana-cc-0001',2,'pagos-api',1794653694,0);
INSERT INTO sessions VALUES(2,'t_sqnkiyau35os','bob-term-01',3,'pagos-api',1794653694,1);
INSERT INTO sessions VALUES(3,'t_dnoyb7q3s4f7','carl-hermes1',4,NULL,1794653694,0);
INSERT INTO sessions VALUES(4,'t_ihvylz6kerxm','mcp_old044',2,NULL,1794653694,0);
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
INSERT INTO participants VALUES('r_or5d7sm54l6v',2,NULL,NULL,NULL,NULL,1,NULL,NULL,NULL,1,0,0,0,0,0,1,NULL,NULL,NULL,0,0);
INSERT INTO participants VALUES('r_or5d7sm54l6v',3,NULL,NULL,NULL,NULL,1,1794653694,'pi','pagos-api',1,1,0,1,1,0,NULL,NULL,NULL,NULL,1,0);
INSERT INTO participants VALUES('r_5y2x7chazch5',4,NULL,NULL,NULL,NULL,2,NULL,NULL,NULL,3,3,1,1,1,0,2,NULL,NULL,NULL,0,0);
INSERT INTO participants VALUES('r_5y2x7chazch5',3,NULL,2,'pagos-api',1794653694,3,1794653694,'pi','pagos-api',3,2,0,0,0,0,3,1794653694,'pi','pagos-api',0,0);
INSERT INTO participants VALUES('r_bga7tgjxwctl',2,NULL,NULL,NULL,NULL,4,NULL,NULL,NULL,4,0,0,0,0,0,4,NULL,NULL,NULL,0,NULL);
INSERT INTO participants VALUES('r_bga7tgjxwctl',3,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,4,4,1,0,1,0,NULL,NULL,NULL,NULL,0,NULL);
INSERT INTO participants VALUES('r_bga7tgjxwctl',4,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,4,4,1,0,1,0,NULL,NULL,NULL,NULL,0,NULL);
INSERT INTO participants VALUES('r_7lt67jckgbsb',2,NULL,NULL,NULL,NULL,5,NULL,NULL,NULL,5,0,0,0,0,0,5,NULL,NULL,NULL,0,0);
INSERT INTO participants VALUES('r_7lt67jckgbsb',3,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,5,5,1,1,1,0,NULL,NULL,NULL,NULL,0,0);
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
INSERT INTO session_threads VALUES(4,'r_or5d7sm54l6v',2,0,0,0,1,NULL,NULL,0,0);
INSERT INTO session_threads VALUES(4,'r_bga7tgjxwctl',2,0,0,0,4,NULL,NULL,0,0);
INSERT INTO session_threads VALUES(4,'r_7lt67jckgbsb',2,0,0,0,5,NULL,NULL,0,0);
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
INSERT INTO notes VALUES(1,'n_k4ai2rzcw6pe','r_or5d7sm54l6v',2,'t_ihvylz6kerxm','Claude Code','@bob Fetched, not answered',1794653694);
INSERT INTO notes VALUES(2,'n_ef4anfwhjdd6','r_5y2x7chazch5',4,'t_dnoyb7q3s4f7','Hermes','@bob Deploy tonight?',1794653694);
INSERT INTO notes VALUES(3,'n_r4yrjuhxu3qq','r_5y2x7chazch5',3,'t_sqnkiyau35os','pi','Yes, at nine.',1794653694);
INSERT INTO notes VALUES(4,'n_u5uoccxrbrob','r_bga7tgjxwctl',2,'t_ihvylz6kerxm','Claude Code','@backend Is staging down?',1794653694);
INSERT INTO notes VALUES(5,'n_4th2dgc5nx7o','r_7lt67jckgbsb',2,'t_ihvylz6kerxm','Claude Code','@bob Never fetched.',1794653694);
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
INSERT INTO send_keys VALUES(2,'k-fetched','r_or5d7sm54l6v','n_k4ai2rzcw6pe','open',NULL,1794653694,'{"kind":"direct","to":{"people":["bob"],"teams":[]},"added":[],"reached":1}');
CREATE TABLE teams (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    title      TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
);
INSERT INTO teams VALUES(1,'backend','Backend',1794653694);
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
INSERT INTO thread_teams VALUES('r_bga7tgjxwctl','backend');
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
DELETE FROM sqlite_sequence;
INSERT INTO sqlite_sequence VALUES('sessions',11);
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
PRAGMA user_version=8;
