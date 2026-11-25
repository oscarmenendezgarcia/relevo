//! `GET /api/dist/...` (docs/update.md): the server hands out the files its manifest lists, to
//! anyone with a valid token, and nothing else.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use relevo::{App, Clock, api, db, dist};
use serde_json::{Value, json};
use tower::ServiceExt;

struct Fixture {
    app: Arc<App>,
    token: String,
    dir: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A server with `ana`, its database file in a directory of its own, and the dist `dist/` next to
/// it (only made when `with_dist`).
fn fixture(with_dist: bool) -> Fixture {
    let dir = std::env::temp_dir().join(db::random_id("relevo-dist-"));
    std::fs::create_dir(&dir).unwrap();
    let db_file = dir.join("relevo.db");
    let dist_dir = dist::dir_for(db_file.to_str().unwrap());
    assert_eq!(dist_dir, dir.join("dist"));
    // Next to the dist, what must never be handed out.
    std::fs::write(&db_file, "the database").unwrap();
    if with_dist {
        std::fs::create_dir(&dist_dir).unwrap();
        let manifest = json!({
            "version": "0.4.0",
            "files": [
                { "name": "relevo-x86_64-unknown-linux-gnu", "kind": "binary",
                  "target": "x86_64-unknown-linux-gnu", "size": 3, "sha256": "00" },
                { "name": "relevo-pi-0.4.0.tar.gz", "kind": "pi", "size": 2, "sha256": "00" },
                { "name": "relevo-hermes-0.4.0.tar.gz", "kind": "hermes", "size": 2, "sha256": "00" },
            ]
        });
        std::fs::write(dist_dir.join("manifest.json"), manifest.to_string()).unwrap();
        std::fs::write(dist_dir.join("manifest.sig"), [7u8; 64]).unwrap();
        std::fs::write(dist_dir.join("relevo-x86_64-unknown-linux-gnu"), "elf").unwrap();
        std::fs::write(dist_dir.join("relevo-pi-0.4.0.tar.gz"), "pi").unwrap();
        // Listed, but missing: relevo-hermes-0.4.0.tar.gz. Present, but not listed:
        std::fs::write(dist_dir.join("notes.txt"), "not listed").unwrap();
        std::fs::create_dir(dist_dir.join("sub")).unwrap();
        std::fs::write(dist_dir.join("sub/x"), "nested").unwrap();
    }
    let app = App::with_dist(
        db::open(":memory:").unwrap(),
        Clock::system(),
        5,
        Some(dist_dir),
        relevo::crypto::Key::random(),
    );
    let token = db::invite(&app.db(), "ana", "Ana", None, "laptop", db::now_unix())
        .unwrap()
        .secret;
    Fixture { app, token, dir }
}

/// The status, the content type and the body of `GET uri`.
async fn get(app: &Arc<App>, uri: &str, token: Option<&str>) -> (StatusCode, String, Vec<u8>) {
    let mut req = Request::builder().method("GET").uri(uri);
    if let Some(token) = token {
        req = req.header("authorization", format!("Bearer {token}"));
    }
    let res = api::router(app.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let kind = res
        .headers()
        .get("content-type")
        .map(|v| v.to_str().unwrap().to_string())
        .unwrap_or_default();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, kind, body)
}

fn code(body: &[u8]) -> String {
    let v: Value = serde_json::from_slice(body).unwrap();
    v["error"]["code"].as_str().unwrap().to_string()
}

#[tokio::test]
async fn the_manifest_its_signature_and_the_listed_files_are_handed_out_as_they_are() {
    let f = fixture(true);
    let token = Some(f.token.as_str());
    let (status, kind, body) = get(&f.app, "/api/dist/manifest.json", token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kind, "application/json");
    let on_disk = std::fs::read(f.dir.join("dist/manifest.json")).unwrap();
    assert_eq!(body, on_disk);

    let (status, kind, body) = get(&f.app, "/api/dist/manifest.sig", token).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(kind, "application/octet-stream");
    assert_eq!(body, [7u8; 64]);

    let (status, _, body) = get(&f.app, "/api/dist/relevo-x86_64-unknown-linux-gnu", token).await;
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, b"elf".as_slice())
    );
    let (status, _, body) = get(&f.app, "/api/dist/relevo-pi-0.4.0.tar.gz", token).await;
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, b"pi".as_slice())
    );
}

#[tokio::test]
async fn a_bigger_file_comes_whole() {
    let f = fixture(true);
    let big: Vec<u8> = (0..1_000_003u32).map(|i| (i * 7 % 251) as u8).collect();
    std::fs::write(f.dir.join("dist/relevo-pi-0.4.0.tar.gz"), &big).unwrap();
    let (status, _, body) = get(&f.app, "/api/dist/relevo-pi-0.4.0.tar.gz", Some(&f.token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body == big, "{} bytes, not {}", body.len(), big.len());
}

#[tokio::test]
async fn every_file_needs_a_valid_token() {
    let f = fixture(true);
    for name in [
        "manifest.json",
        "manifest.sig",
        "relevo-x86_64-unknown-linux-gnu",
        "nothing",
    ] {
        let uri = format!("/api/dist/{name}");
        let (status, _, body) = get(&f.app, &uri, None).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{name}");
        assert_eq!(code(&body), "UNAUTHORIZED");
        let (status, _, _) = get(&f.app, &uri, Some("rlv_not-a-token")).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{name}");
    }
    // A revoked token too.
    let id = db::list_tokens(&f.app.db(), Some("ana")).unwrap()[0]
        .id
        .clone();
    db::revoke_token(&f.app.db(), &id, db::now_unix()).unwrap();
    let (status, _, _) = get(&f.app, "/api/dist/manifest.json", Some(&f.token)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn only_what_the_manifest_lists_and_nothing_outside_the_dist() {
    let f = fixture(true);
    let token = Some(f.token.as_str());
    for uri in [
        // In the directory, but not listed.
        "/api/dist/notes.txt",
        "/api/dist/sub",
        "/api/dist/sub/x",
        // Listed, but not there.
        "/api/dist/relevo-hermes-0.4.0.tar.gz",
        // Outside it, however it is spelled.
        "/api/dist/../relevo.db",
        "/api/dist/..%2Frelevo.db",
        "/api/dist/%2E%2E%2Frelevo.db",
        "/api/dist/%2E%2E",
        "/api/dist/..",
        "/api/dist/.",
        "/api/dist/%2Fetc%2Fpasswd",
        "/api/dist/",
        "/api/dist",
    ] {
        let (status, _, body) = get(&f.app, uri, token).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(code(&body), "NOT_FOUND", "{uri}");
        assert!(
            !String::from_utf8_lossy(&body).contains("the database"),
            "{uri}"
        );
    }
    // A manifest that names something outside lists nothing that is served.
    let manifest = json!({ "version": "0.4.0", "files": [
        { "name": "../relevo.db", "kind": "binary", "size": 12, "sha256": "00" } ] });
    std::fs::write(f.dir.join("dist/manifest.json"), manifest.to_string()).unwrap();
    let (status, _, _) = get(&f.app, "/api/dist/..%2Frelevo.db", token).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // Another method is not a route.
    let res = api::router(f.app.clone())
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri("/api/dist/manifest.json")
                .header("authorization", format!("Bearer {}", f.token))
                .body(Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn without_a_dist_everything_is_not_found() {
    // No dist/ directory next to the database.
    let f = fixture(false);
    for name in [
        "manifest.json",
        "manifest.sig",
        "relevo-x86_64-unknown-linux-gnu",
    ] {
        let (status, _, body) = get(&f.app, &format!("/api/dist/{name}"), Some(&f.token)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{name}");
        assert_eq!(code(&body), "NOT_FOUND");
    }
    // Nor with no dist configured at all.
    let app = App::new(db::open(":memory:").unwrap(), Clock::system(), 5);
    let token = db::invite(&app.db(), "ana", "Ana", None, "x", db::now_unix())
        .unwrap()
        .secret;
    let (status, _, _) = get(&app, "/api/dist/manifest.json", Some(&token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[test]
fn serve_warns_at_start_when_the_dist_is_not_its_version() {
    let f = fixture(true);
    let dir = f.dir.join("dist");
    let warning = dist::start_note(&dir, "0.3.0").unwrap_err();
    assert!(
        warning.contains("version 0.4.0, not this server's 0.3.0"),
        "{warning}"
    );
    let note = dist::start_note(&dir, "0.4.0").unwrap();
    assert!(note.contains("0.4.0"), "{note}");
    // No signature: the clients would refuse it.
    std::fs::remove_file(dir.join("manifest.sig")).unwrap();
    let warning = dist::start_note(&dir, "0.4.0").unwrap_err();
    assert!(warning.contains("manifest.sig"), "{warning}");
    // No dist is fine, and said.
    let note = dist::start_note(Path::new(&f.dir.join("none")), "0.4.0").unwrap();
    assert!(note.contains("no dist"), "{note}");
    // A manifest that is not one.
    std::fs::write(dir.join("manifest.json"), "<html>").unwrap();
    assert!(dist::start_note(&dir, "0.4.0").is_err());
}
