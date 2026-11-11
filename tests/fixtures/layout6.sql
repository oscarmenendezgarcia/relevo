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
INSERT INTO people VALUES(1,'ana','Ana','payments backend',1,1794393850);
INSERT INTO people VALUES(2,'bob','Bob','infrastructure',1,1794393850);
INSERT INTO people VALUES(3,'carl','Carl','billing',1,1794393850);
CREATE TABLE tokens (
    id         TEXT PRIMARY KEY,
    person_id  INTEGER NOT NULL REFERENCES people(id),
    label      TEXT NOT NULL,
    hash       TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER
);
INSERT INTO tokens VALUES('t_kqdvnx53dvqt',1,'Claude Code','af83590f19a51c73575840f5a86e340491750f2b426cf497b17773fe4a27a979',1794393850,NULL);
INSERT INTO tokens VALUES('t_z6qavboe4asi',2,'pi','23361bcc1198ff08775831cc56632892f6d697641674e4f83cb755537f2825fc',1794393850,NULL);
INSERT INTO tokens VALUES('t_hgsebpicn6kv',3,'Hermes','a7f0d6b17dc119684b76e44caec1778394c74e4cd2984abb6548de738842edb7',1794393850,NULL);
CREATE TABLE threads (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('direct', 'team')),
    title      TEXT NOT NULL,
    opener_id  INTEGER NOT NULL REFERENCES people(id),
    status     TEXT NOT NULL CHECK (status IN ('open', 'closed')),
    created_at INTEGER NOT NULL
);
INSERT INTO threads VALUES('r_pxsxyijycdc5','direct','Fetched, not answered',1,'open',1794393850);
INSERT INTO threads VALUES('r_26jmvdxtfgsv','direct','Replied?',1,'open',1794393850);
INSERT INTO threads VALUES('r_jjjrxezton6r','direct','FYI, deployed.',3,'open',1794393850);
INSERT INTO threads VALUES('r_duekqcrvmt36','team','Who owns billing?',1,'open',1794393850);
INSERT INTO threads VALUES('r_3qwkprknqzko','team','Anyone on call tonight?',3,'open',1794393850);
INSERT INTO threads VALUES('r_awxyxjmiqiz5','team','Is staging down?',1,'closed',1794393850);
INSERT INTO threads VALUES('r_kgbm3jdprxjj','direct','Claim me',3,'open',1794393850);
INSERT INTO threads VALUES('r_xbsqqdtx2ocw','direct','Never fetched.',1,'open',1794393850);
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
INSERT INTO sessions VALUES(1,'t_z6qavboe4asi','bob-term-02',2,NULL,1794393850,0);
INSERT INTO sessions VALUES(2,'t_kqdvnx53dvqt','ana-cc-0001',1,'pagos-api',1794393850,1);
INSERT INTO sessions VALUES(3,'t_z6qavboe4asi','bob-term-01',2,'pagos-api',1794393850,1);
INSERT INTO sessions VALUES(7,'t_hgsebpicn6kv','carl-hermes1',3,NULL,1794393850,1);
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
    PRIMARY KEY (thread_id, person_id)
);
INSERT INTO participants VALUES('r_pxsxyijycdc5',1,NULL,NULL,NULL,NULL,1,NULL,NULL,NULL,1,0,0,0,0,0,1,NULL,NULL,NULL,0);
INSERT INTO participants VALUES('r_pxsxyijycdc5',2,NULL,NULL,NULL,NULL,1,1794393850,'pi','pagos-api',1,1,0,1,1,0,NULL,NULL,NULL,NULL,1);
INSERT INTO participants VALUES('r_26jmvdxtfgsv',1,NULL,NULL,NULL,NULL,3,1794393850,'Claude Code','pagos-api',3,3,0,1,1,0,2,NULL,NULL,NULL,1);
INSERT INTO participants VALUES('r_26jmvdxtfgsv',2,NULL,3,'pagos-api',1794393850,3,1794393850,'pi','pagos-api',3,2,0,0,0,0,3,1794393850,'pi','pagos-api',0);
INSERT INTO participants VALUES('r_jjjrxezton6r',3,NULL,NULL,NULL,NULL,4,NULL,NULL,NULL,4,0,0,0,0,0,4,NULL,NULL,NULL,0);
INSERT INTO participants VALUES('r_jjjrxezton6r',2,NULL,NULL,NULL,NULL,4,1794393850,'pi','pagos-api',4,4,0,0,0,0,4,1794393850,'pi','pagos-api',0);
INSERT INTO participants VALUES('r_duekqcrvmt36',1,NULL,NULL,NULL,NULL,6,1794393850,'Claude Code','pagos-api',6,6,0,0,0,0,5,NULL,NULL,NULL,0);
INSERT INTO participants VALUES('r_duekqcrvmt36',2,NULL,NULL,NULL,NULL,6,1794393850,'pi','pagos-api',6,6,0,0,0,0,NULL,NULL,NULL,NULL,0);
INSERT INTO participants VALUES('r_duekqcrvmt36',3,NULL,7,NULL,1794393850,6,1794393850,'Hermes',NULL,6,5,0,0,0,0,6,1794393850,'Hermes',NULL,0);
INSERT INTO participants VALUES('r_3qwkprknqzko',3,NULL,NULL,NULL,NULL,7,NULL,NULL,NULL,7,0,0,0,0,0,7,NULL,NULL,NULL,0);
INSERT INTO participants VALUES('r_3qwkprknqzko',1,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,7,7,1,0,1,0,NULL,NULL,NULL,NULL,0);
INSERT INTO participants VALUES('r_3qwkprknqzko',2,NULL,NULL,NULL,NULL,7,1794393850,'pi','pagos-api',7,7,0,0,0,0,NULL,NULL,NULL,NULL,0);
INSERT INTO participants VALUES('r_awxyxjmiqiz5',1,NULL,NULL,NULL,NULL,9,NULL,NULL,NULL,9,0,0,0,0,1,9,NULL,NULL,NULL,0);
INSERT INTO participants VALUES('r_awxyxjmiqiz5',2,NULL,NULL,NULL,NULL,8,1794393850,'pi','pagos-api',9,9,1,0,1,1,NULL,NULL,NULL,NULL,0);
INSERT INTO participants VALUES('r_awxyxjmiqiz5',3,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,9,9,1,0,1,1,NULL,NULL,NULL,NULL,0);
INSERT INTO participants VALUES('r_kgbm3jdprxjj',3,NULL,NULL,NULL,NULL,10,NULL,NULL,NULL,10,0,0,0,0,0,10,NULL,NULL,NULL,0);
INSERT INTO participants VALUES('r_kgbm3jdprxjj',2,NULL,3,'pagos-api',1794393850,10,1794393850,'pi','pagos-api',10,10,0,1,1,0,NULL,NULL,NULL,NULL,1);
INSERT INTO participants VALUES('r_xbsqqdtx2ocw',1,NULL,NULL,NULL,NULL,11,NULL,NULL,NULL,11,0,0,0,0,0,11,NULL,NULL,NULL,0);
INSERT INTO participants VALUES('r_xbsqqdtx2ocw',2,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,11,11,1,1,1,0,NULL,NULL,NULL,NULL,0);
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
INSERT INTO session_threads VALUES(1,'r_pxsxyijycdc5',2,0,1,1,1,NULL,NULL,1,0);
INSERT INTO session_threads VALUES(1,'r_26jmvdxtfgsv',2,0,1,0,3,3,NULL,0,0);
INSERT INTO session_threads VALUES(1,'r_jjjrxezton6r',2,0,1,0,4,NULL,NULL,0,0);
INSERT INTO session_threads VALUES(1,'r_awxyxjmiqiz5',2,0,1,0,9,NULL,NULL,1,1);
INSERT INTO session_threads VALUES(1,'r_3qwkprknqzko',2,0,1,0,7,NULL,NULL,0,0);
INSERT INTO session_threads VALUES(1,'r_duekqcrvmt36',2,0,1,0,6,NULL,NULL,0,0);
INSERT INTO session_threads VALUES(1,'r_kgbm3jdprxjj',2,0,1,1,10,3,NULL,1,0);
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
INSERT INTO notes VALUES(1,'n_txtix7mllafq','r_pxsxyijycdc5',1,'t_kqdvnx53dvqt','Claude Code','Fetched, not answered',1794393850);
INSERT INTO notes VALUES(2,'n_miuswggwwxvm','r_26jmvdxtfgsv',1,'t_kqdvnx53dvqt','Claude Code','Replied?',1794393850);
INSERT INTO notes VALUES(3,'n_fag7j2ykeqnj','r_26jmvdxtfgsv',2,'t_z6qavboe4asi','pi','Yes.',1794393850);
INSERT INTO notes VALUES(4,'n_7ukote4qtxhs','r_jjjrxezton6r',3,'t_hgsebpicn6kv','Hermes','FYI, deployed.',1794393850);
INSERT INTO notes VALUES(5,'n_pt6o7yanaa2s','r_duekqcrvmt36',1,'t_kqdvnx53dvqt','Claude Code','Who owns billing?',1794393850);
INSERT INTO notes VALUES(6,'n_4rbw2ez5udzo','r_duekqcrvmt36',3,'t_hgsebpicn6kv','Hermes','Me.',1794393850);
INSERT INTO notes VALUES(7,'n_czwb7et32wiy','r_3qwkprknqzko',3,'t_hgsebpicn6kv','Hermes','Anyone on call tonight?',1794393850);
INSERT INTO notes VALUES(8,'n_thzn3zsubqom','r_awxyxjmiqiz5',1,'t_kqdvnx53dvqt','Claude Code','Is staging down?',1794393850);
INSERT INTO notes VALUES(9,'n_vjh73hlvxltu','r_awxyxjmiqiz5',1,'t_kqdvnx53dvqt','Claude Code','Back up, thanks.',1794393850);
INSERT INTO notes VALUES(10,'n_tug2327a7zdy','r_kgbm3jdprxjj',3,'t_hgsebpicn6kv','Hermes','Claim me',1794393850);
INSERT INTO notes VALUES(11,'n_hvpwul2mridz','r_xbsqqdtx2ocw',1,'t_kqdvnx53dvqt','Claude Code','Never fetched.',1794393850);
CREATE TABLE send_keys (
    person_id  INTEGER NOT NULL REFERENCES people(id),
    key        TEXT NOT NULL,
    thread_id  TEXT NOT NULL,
    note_id    TEXT,
    status     TEXT NOT NULL,
    claimed_by INTEGER,
    created_at INTEGER NOT NULL,
    PRIMARY KEY (person_id, key)
);
INSERT INTO send_keys VALUES(1,'k-fetched','r_pxsxyijycdc5','n_txtix7mllafq','open',NULL,1794393850);
DELETE FROM sqlite_sequence;
INSERT INTO sqlite_sequence VALUES('sessions',21);
INSERT INTO sqlite_sequence VALUES('notes',11);
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
COMMIT;
PRAGMA user_version=6;
