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
INSERT INTO people VALUES(1,'ana','Ana','payments backend',1,1794598513);
INSERT INTO people VALUES(2,'bob','Bob','infrastructure',1,1794598513);
INSERT INTO people VALUES(3,'carl','Carl','billing',1,1794598513);
CREATE TABLE tokens (
    id         TEXT PRIMARY KEY,
    person_id  INTEGER NOT NULL REFERENCES people(id),
    label      TEXT NOT NULL,
    hash       TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER
);
INSERT INTO tokens VALUES('t_32j3g4toodki',1,'Claude Code','52713cddb3da0be30135218f216b76934e4f1047ff19c50a6c0be145af5fd0c1',1794598513,NULL);
INSERT INTO tokens VALUES('t_vvp7o6zivs3w',2,'pi','c75a9e58783904e5c0d4f0b8a43b9e19b127b40d9e54f9115532596e28e0eefc',1794598513,NULL);
INSERT INTO tokens VALUES('t_rmg37mrgv5um',3,'Hermes','994ca6bb245d0f2c689932197dc3ce490df958578649fb6fc2829a9a8499f8ae',1794598513,NULL);
CREATE TABLE threads (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('direct', 'team', 'global')),
    title      TEXT NOT NULL,
    opener_id  INTEGER NOT NULL REFERENCES people(id),
    status     TEXT NOT NULL CHECK (status IN ('open', 'closed')),
    created_at INTEGER NOT NULL
);
INSERT INTO threads VALUES('r_464dk6p7ecau','direct','@bob Fetched, not answered',1,'open',1794598513);
INSERT INTO threads VALUES('r_ivjwfa7tnpba','direct','@bob FYI, deployed.',3,'open',1794598513);
INSERT INTO threads VALUES('r_x6gagvtksbsa','direct','@bob Deploy tonight?',1,'closed',1794598513);
INSERT INTO threads VALUES('r_v4che2powxax','team','@backend @carl Is staging down?',1,'closed',1794598513);
INSERT INTO threads VALUES('r_suepzv543qnm','global','Anyone on call tonight?',3,'closed',1794598513);
INSERT INTO threads VALUES('r_34tygyqtap3c','direct','@bob Never fetched.',1,'open',1794598513);
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
INSERT INTO sessions VALUES(1,'t_32j3g4toodki','ana-cc-0001',1,'pagos-api',1794598513,1);
INSERT INTO sessions VALUES(2,'t_vvp7o6zivs3w','bob-term-01',2,'pagos-api',1794598513,1);
INSERT INTO sessions VALUES(3,'t_rmg37mrgv5um','carl-hermes1',3,NULL,1794598513,0);
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
INSERT INTO participants VALUES('r_464dk6p7ecau',1,NULL,NULL,NULL,NULL,1,NULL,NULL,NULL,1,0,0,0,0,0,1,NULL,NULL,NULL,0,0);
INSERT INTO participants VALUES('r_464dk6p7ecau',2,NULL,NULL,NULL,NULL,1,1794598513,'pi','pagos-api',1,1,0,1,1,0,NULL,NULL,NULL,NULL,1,0);
INSERT INTO participants VALUES('r_ivjwfa7tnpba',3,NULL,NULL,NULL,NULL,2,NULL,NULL,NULL,2,0,0,0,0,0,2,NULL,NULL,NULL,0,0);
INSERT INTO participants VALUES('r_ivjwfa7tnpba',2,NULL,NULL,NULL,NULL,2,1794598513,'pi','pagos-api',2,2,0,0,0,0,2,1794598513,'pi','pagos-api',0,0);
INSERT INTO participants VALUES('r_x6gagvtksbsa',1,NULL,NULL,NULL,NULL,3,NULL,NULL,NULL,4,4,1,0,1,0,3,NULL,NULL,NULL,0,0);
INSERT INTO participants VALUES('r_x6gagvtksbsa',2,NULL,NULL,NULL,NULL,4,1794598513,'pi','pagos-api',4,3,0,0,0,0,4,1794598513,'pi','pagos-api',0,0);
INSERT INTO participants VALUES('r_v4che2powxax',1,NULL,NULL,NULL,NULL,6,NULL,NULL,NULL,6,0,0,0,0,1,6,NULL,NULL,NULL,0,NULL);
INSERT INTO participants VALUES('r_v4che2powxax',3,NULL,NULL,NULL,NULL,6,1794598513,'Hermes',NULL,6,6,0,0,0,0,NULL,NULL,NULL,NULL,0,0);
INSERT INTO participants VALUES('r_v4che2powxax',2,NULL,NULL,NULL,NULL,5,1794598513,'pi','pagos-api',6,6,1,0,1,1,NULL,NULL,NULL,NULL,0,NULL);
INSERT INTO participants VALUES('r_suepzv543qnm',3,NULL,NULL,NULL,NULL,8,1794598513,'Hermes',NULL,8,8,0,0,0,1,8,1794598513,'Hermes',NULL,0,NULL);
INSERT INTO participants VALUES('r_suepzv543qnm',1,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,8,8,1,0,1,1,NULL,NULL,NULL,NULL,0,NULL);
INSERT INTO participants VALUES('r_suepzv543qnm',2,NULL,NULL,NULL,NULL,8,1794598513,'pi','pagos-api',8,7,0,0,0,1,8,1794598513,'pi','pagos-api',0,NULL);
INSERT INTO participants VALUES('r_34tygyqtap3c',1,NULL,NULL,NULL,NULL,9,NULL,NULL,NULL,9,0,0,0,0,0,9,NULL,NULL,NULL,0,0);
INSERT INTO participants VALUES('r_34tygyqtap3c',2,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,9,9,1,1,1,0,NULL,NULL,NULL,NULL,0,0);
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
INSERT INTO notes VALUES(1,'n_ndpxpqulyckt','r_464dk6p7ecau',1,'t_32j3g4toodki','Claude Code','@bob Fetched, not answered',1794598513);
INSERT INTO notes VALUES(2,'n_mubdi3rq4qmw','r_ivjwfa7tnpba',3,'t_rmg37mrgv5um','Hermes','@bob FYI, deployed.',1794598513);
INSERT INTO notes VALUES(3,'n_n5k7ved2nwww','r_x6gagvtksbsa',1,'t_32j3g4toodki','Claude Code','@bob Deploy tonight?',1794598513);
INSERT INTO notes VALUES(4,'n_2lk6q2xtdd3x','r_x6gagvtksbsa',2,'t_vvp7o6zivs3w','pi','Yes, at nine.',1794598513);
INSERT INTO notes VALUES(5,'n_edkor2g55676','r_v4che2powxax',1,'t_32j3g4toodki','Claude Code','@backend @carl Is staging down?',1794598513);
INSERT INTO notes VALUES(6,'n_335z4cltfg73','r_v4che2powxax',1,'t_32j3g4toodki','Claude Code','Back up, thanks.',1794598513);
INSERT INTO notes VALUES(7,'n_o6uika6smj2z','r_suepzv543qnm',3,'t_rmg37mrgv5um','Hermes','Anyone on call tonight?',1794598513);
INSERT INTO notes VALUES(8,'n_vggokdmxcfs7','r_suepzv543qnm',2,'t_vvp7o6zivs3w','pi','Me.',1794598513);
INSERT INTO notes VALUES(9,'n_navefdnbhuvv','r_34tygyqtap3c',1,'t_32j3g4toodki','Claude Code','@bob Never fetched.',1794598513);
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
INSERT INTO send_keys VALUES(1,'k-fetched','r_464dk6p7ecau','n_ndpxpqulyckt','open',NULL,1794598513,'{"kind":"direct","to":{"people":["bob"],"teams":[]},"added":[],"reached":1}');
INSERT INTO send_keys VALUES(1,'k-close','r_x6gagvtksbsa',NULL,'closed',NULL,1794598513,'{"kind":"direct","to":{"people":[],"teams":[]},"added":[],"reached":1}');
CREATE TABLE teams (
    id         INTEGER PRIMARY KEY,
    name       TEXT NOT NULL UNIQUE,
    title      TEXT NOT NULL DEFAULT '',
    created_at INTEGER NOT NULL
);
INSERT INTO teams VALUES(1,'backend','Backend',1794598513);
CREATE TABLE team_members (
    team_id   INTEGER NOT NULL REFERENCES teams(id),
    person_id INTEGER NOT NULL REFERENCES people(id),
    PRIMARY KEY (team_id, person_id)
);
INSERT INTO team_members VALUES(1,2);
INSERT INTO team_members VALUES(1,3);
CREATE TABLE thread_teams (
    thread_id TEXT NOT NULL REFERENCES threads(id),
    name      TEXT NOT NULL,
    PRIMARY KEY (thread_id, name)
);
INSERT INTO thread_teams VALUES('r_v4che2powxax','backend');
CREATE TABLE note_contexts (
    seq     INTEGER PRIMARY KEY REFERENCES notes(seq),
    context TEXT NOT NULL
);
INSERT INTO note_contexts VALUES(1,'pagos-api');
INSERT INTO note_contexts VALUES(3,'pagos-api');
INSERT INTO note_contexts VALUES(4,'pagos-api');
INSERT INTO note_contexts VALUES(5,'pagos-api');
INSERT INTO note_contexts VALUES(6,'pagos-api');
INSERT INTO note_contexts VALUES(8,'pagos-api');
INSERT INTO note_contexts VALUES(9,'pagos-api');
CREATE TABLE thread_closes (
    thread_id TEXT PRIMARY KEY REFERENCES threads(id),
    person_id INTEGER NOT NULL REFERENCES people(id),
    closed_at INTEGER NOT NULL
);
INSERT INTO thread_closes VALUES('r_x6gagvtksbsa',1,1794598513);
INSERT INTO thread_closes VALUES('r_v4che2powxax',1,1794598513);
INSERT INTO thread_closes VALUES('r_suepzv543qnm',3,1794598513);
DELETE FROM sqlite_sequence;
INSERT INTO sqlite_sequence VALUES('sessions',21);
INSERT INTO sqlite_sequence VALUES('notes',9);
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
    ON participants(person_id, claimed_by, has_new, waiting, last_seq)
    WHERE inbox = 1 AND resolved = 0;
CREATE INDEX participants_inbox_context
    ON participants(person_id, claimed_by, context, has_new, waiting, last_seq)
    WHERE inbox = 1 AND resolved = 0;
CREATE INDEX session_new
    ON session_threads(session_id, waiting, last_seq) WHERE has_new = 1;
CREATE INDEX session_new_claim
    ON session_threads(session_id, claimed_by, waiting, last_seq)
    WHERE has_new = 1 AND resolved = 0;
CREATE INDEX session_new_context
    ON session_threads(session_id, claimed_by, context, waiting, last_seq)
    WHERE has_new = 1 AND resolved = 0;
CREATE INDEX session_count
    ON session_threads(session_id, resolved, claimed_by, context)
    WHERE has_new = 1 AND person_inbox = 0;
CREATE INDEX notes_by_thread ON notes(thread_id, seq);
CREATE INDEX notes_by_author ON notes(author_id, created_at);
CREATE INDEX send_keys_by_time ON send_keys(created_at);
CREATE INDEX team_members_by_person ON team_members(person_id);
CREATE INDEX participants_timeline ON participants(person_id, last_seq);
CREATE INDEX notes_by_thread_author ON notes(thread_id, author_id);
COMMIT;
PRAGMA user_version=7;
