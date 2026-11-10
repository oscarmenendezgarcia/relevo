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
INSERT INTO people VALUES(1,'ana','Ana','payments backend',1,1794255167);
INSERT INTO people VALUES(2,'bob','Bob','infrastructure',1,1794255167);
INSERT INTO people VALUES(3,'carl','Carl','billing',1,1794255167);
CREATE TABLE tokens (
    id         TEXT PRIMARY KEY,
    person_id  INTEGER NOT NULL REFERENCES people(id),
    label      TEXT NOT NULL,
    hash       TEXT NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    revoked_at INTEGER
);
INSERT INTO tokens VALUES('t_kezpatrrjj7i',1,'Claude Code','2d18395a61377e413b3314dc3d6b05c34df90615335d4809c7b9839da14546c9',1794255167,NULL);
INSERT INTO tokens VALUES('t_cmmgtbyo7ah3',2,'pi','66130cfe2d9c0a08fce43562558881465d1055fb3cbe651890cc7b5e464b33d3',1794255167,NULL);
INSERT INTO tokens VALUES('t_xyct23yzk2vo',3,'Hermes','d55486a399aca75aa6743e497a76c1f7beebcf2dbaaa61c58a5697e2f8fe9915',1794255167,NULL);
CREATE TABLE threads (
    id         TEXT PRIMARY KEY,
    kind       TEXT NOT NULL CHECK (kind IN ('direct', 'team')),
    title      TEXT NOT NULL,
    opener_id  INTEGER NOT NULL REFERENCES people(id),
    status     TEXT NOT NULL CHECK (status IN ('open', 'closed')),
    created_at INTEGER NOT NULL
);
INSERT INTO threads VALUES('r_26pub3tz5pth','direct','Fetched, not answered',1,'open',1794255167);
INSERT INTO threads VALUES('r_n32c26amdtqx','direct','Replied?',1,'open',1794255167);
INSERT INTO threads VALUES('r_5unakjhib3jh','direct','Blind',1,'open',1794255167);
INSERT INTO threads VALUES('r_ez7oazoqlhmf','team','Who owns billing?',1,'open',1794255167);
INSERT INTO threads VALUES('r_46rw6zx3dft6','direct','Closing',1,'closed',1794255167);
INSERT INTO threads VALUES('r_jvd5vyhw2qhb','direct','Claim me',3,'open',1794255167);
INSERT INTO threads VALUES('r_5ilk6yxppojm','direct','Never fetched.',1,'open',1794255167);
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
INSERT INTO sessions VALUES(1,'t_cmmgtbyo7ah3','bob-term-02',2,NULL,1794255167,1);
INSERT INTO sessions VALUES(2,'t_kezpatrrjj7i','ana-cc-0001',1,'pagos-api',1794255167,1);
INSERT INTO sessions VALUES(3,'t_cmmgtbyo7ah3','bob-term-01',2,'pagos-api',1794255167,1);
INSERT INTO sessions VALUES(10,'t_xyct23yzk2vo','carl-hermes1',3,NULL,1794255167,1);
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
    PRIMARY KEY (thread_id, person_id)
);
INSERT INTO participants VALUES('r_26pub3tz5pth',1,NULL,NULL,NULL,NULL,1,NULL,NULL,NULL,1,0,0,0,0,0);
INSERT INTO participants VALUES('r_26pub3tz5pth',2,NULL,NULL,NULL,NULL,1,1794255167,'pi','pagos-api',1,1,0,1,1,0);
INSERT INTO participants VALUES('r_n32c26amdtqx',1,NULL,NULL,NULL,NULL,3,1794255167,'Claude Code','pagos-api',3,3,0,1,1,0);
INSERT INTO participants VALUES('r_n32c26amdtqx',2,NULL,3,'pagos-api',1794255167,3,1794255167,'pi','pagos-api',3,2,0,0,0,0);
INSERT INTO participants VALUES('r_5unakjhib3jh',1,NULL,NULL,NULL,NULL,5,1794255167,'Claude Code','pagos-api',5,5,0,1,1,0);
INSERT INTO participants VALUES('r_5unakjhib3jh',2,NULL,1,NULL,1794255167,NULL,NULL,NULL,NULL,5,4,1,0,1,0);
INSERT INTO participants VALUES('r_ez7oazoqlhmf',1,NULL,NULL,NULL,NULL,7,1794255167,'Claude Code','pagos-api',7,7,0,0,0,0);
INSERT INTO participants VALUES('r_ez7oazoqlhmf',2,NULL,NULL,NULL,NULL,7,1794255167,'pi','pagos-api',7,7,0,0,0,0);
INSERT INTO participants VALUES('r_ez7oazoqlhmf',3,NULL,10,NULL,1794255167,7,1794255167,'Hermes',NULL,7,6,0,0,0,0);
INSERT INTO participants VALUES('r_46rw6zx3dft6',1,NULL,NULL,NULL,NULL,9,NULL,NULL,NULL,9,0,0,0,0,0);
INSERT INTO participants VALUES('r_46rw6zx3dft6',2,NULL,NULL,NULL,NULL,9,1794255167,'pi','pagos-api',9,9,0,0,0,0);
INSERT INTO participants VALUES('r_jvd5vyhw2qhb',3,NULL,NULL,NULL,NULL,10,NULL,NULL,NULL,10,0,0,0,0,0);
INSERT INTO participants VALUES('r_jvd5vyhw2qhb',2,NULL,3,'pagos-api',1794255167,10,1794255167,'pi','pagos-api',10,10,0,1,1,0);
INSERT INTO participants VALUES('r_5ilk6yxppojm',1,NULL,NULL,NULL,NULL,11,NULL,NULL,NULL,11,0,0,0,0,0);
INSERT INTO participants VALUES('r_5ilk6yxppojm',2,NULL,NULL,NULL,NULL,NULL,NULL,NULL,NULL,11,11,1,1,1,0);
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
INSERT INTO session_threads VALUES(1,'r_26pub3tz5pth',2,0,1,1,1,NULL,NULL,1,0);
INSERT INTO session_threads VALUES(1,'r_n32c26amdtqx',2,0,1,0,3,3,NULL,0,0);
INSERT INTO session_threads VALUES(1,'r_46rw6zx3dft6',2,0,1,0,9,NULL,NULL,0,0);
INSERT INTO session_threads VALUES(1,'r_ez7oazoqlhmf',2,0,1,0,7,NULL,NULL,0,0);
INSERT INTO session_threads VALUES(1,'r_jvd5vyhw2qhb',2,0,1,1,10,3,NULL,1,0);
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
INSERT INTO notes VALUES(1,'n_l5j3brccc6or','r_26pub3tz5pth',1,'t_kezpatrrjj7i','Claude Code','Fetched, not answered',1794255167);
INSERT INTO notes VALUES(2,'n_tekqmmojnsqg','r_n32c26amdtqx',1,'t_kezpatrrjj7i','Claude Code','Replied?',1794255167);
INSERT INTO notes VALUES(3,'n_yptnybealgru','r_n32c26amdtqx',2,'t_cmmgtbyo7ah3','pi','Yes.',1794255167);
INSERT INTO notes VALUES(4,'n_daof5rh4x5a5','r_5unakjhib3jh',1,'t_kezpatrrjj7i','Claude Code','Blind',1794255167);
INSERT INTO notes VALUES(5,'n_laln4vgtbugi','r_5unakjhib3jh',2,'t_cmmgtbyo7ah3','pi','Answer without reading.',1794255167);
INSERT INTO notes VALUES(6,'n_sfinxdo6gcg5','r_ez7oazoqlhmf',1,'t_kezpatrrjj7i','Claude Code','Who owns billing?',1794255167);
INSERT INTO notes VALUES(7,'n_24l2ptaplkwc','r_ez7oazoqlhmf',3,'t_xyct23yzk2vo','Hermes','Me.',1794255167);
INSERT INTO notes VALUES(8,'n_qsasfcltmefs','r_46rw6zx3dft6',1,'t_kezpatrrjj7i','Claude Code','Closing',1794255167);
INSERT INTO notes VALUES(9,'n_ezujdtgev7dz','r_46rw6zx3dft6',1,'t_kezpatrrjj7i','Claude Code','Done, closing.',1794255167);
INSERT INTO notes VALUES(10,'n_dzykuei63syz','r_jvd5vyhw2qhb',3,'t_xyct23yzk2vo','Hermes','Claim me',1794255167);
INSERT INTO notes VALUES(11,'n_rbhmvnl7z6jn','r_5ilk6yxppojm',1,'t_kezpatrrjj7i','Claude Code','Never fetched.',1794255167);
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
DELETE FROM sqlite_sequence;
INSERT INTO sqlite_sequence VALUES('sessions',19);
INSERT INTO sqlite_sequence VALUES('notes',11);
CREATE INDEX sessions_by_person ON sessions(person_id, last_seen);
CREATE INDEX sessions_by_last_seen ON sessions(last_seen);
CREATE INDEX sessions_claiming ON sessions(last_seen) WHERE claims = 1;
CREATE INDEX threads_by_opener ON threads(opener_id, kind, created_at);
CREATE INDEX session_threads_by_thread ON session_threads(thread_id);
CREATE INDEX session_threads_by_claim
    ON session_threads(claimed_by) WHERE claimed_by IS NOT NULL;
CREATE INDEX participants_by_person ON participants(person_id);
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
PRAGMA user_version=5;
