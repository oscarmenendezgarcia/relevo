//! TLS with rustls: the certificate `relevo serve --tls` makes and keeps next to the database,
//! what `relevo connect` sees of a server's certificate, and the pinned certificate a client
//! trusts alone once `connect` has saved it.

use std::io::Write;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, WebPkiSupportedAlgorithms};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{CertificateError, DigitallySignedStruct, SignatureScheme};
use sha2::{Digest, Sha256};

/// How long a self-signed certificate is valid.
const VALID_DAYS: i64 = 3650;

fn provider() -> Arc<CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// The SHA-256 of a certificate (DER), as colon-separated upper-case hex.
pub fn fingerprint(cert: &CertificateDer) -> String {
    Sha256::digest(cert.as_ref())
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// The pin of a certificate's key, as `curl --pinnedpubkey sha256//<pin>` takes it: the base64
/// SHA-256 of its SubjectPublicKeyInfo (DER).
pub fn key_pin(cert: &CertificateDer) -> Result<String> {
    use base64::Engine;
    let cert = webpki::EndEntityCert::try_from(cert)
        .map_err(|e| anyhow!("reading the certificate: {e}"))?;
    let spki = cert.subject_public_key_info();
    Ok(base64::engine::general_purpose::STANDARD.encode(Sha256::digest(spki.as_ref())))
}

/// Whether `given` (hex, with or without colons or spaces, any case) is `fingerprint`.
pub fn same_fingerprint(given: &str, fingerprint: &str) -> bool {
    let hex = |s: &str| -> String {
        s.chars()
            .filter(|c| !matches!(c, ':' | ' '))
            .collect::<String>()
            .to_ascii_uppercase()
    };
    let given = hex(given);
    !given.is_empty() && given == hex(fingerprint)
}

/// `cert` as PEM.
pub fn pem(cert: &CertificateDer) -> String {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(cert.as_ref());
    let mut out = String::from("-----BEGIN CERTIFICATE-----\n");
    for line in b64.as_bytes().chunks(64) {
        out.push_str(std::str::from_utf8(line).expect("base64 is ASCII"));
        out.push('\n');
    }
    out.push_str("-----END CERTIFICATE-----\n");
    out
}

/// The certificates of a PEM file's text, in order; at least one.
pub fn certs_from_pem(pem: &str) -> Result<Vec<CertificateDer<'static>>> {
    let certs = CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| anyhow!("not a PEM certificate: {e}"))?;
    if certs.is_empty() {
        bail!("no certificate in the PEM text");
    }
    Ok(certs)
}

/// A host name or IP address as TLS checks it.
fn server_name(name: &str) -> Result<ServerName<'static>> {
    let bare = name.trim_start_matches('[').trim_end_matches(']');
    ServerName::try_from(bare.to_string())
        .map_err(|_| anyhow!("not a host name or IP address: {name}"))
}

/// Whether `cert` names `name` (a host name or an IP address).
pub fn names(cert: &CertificateDer, name: &ServerName) -> bool {
    webpki::EndEntityCert::try_from(cert)
        .is_ok_and(|ee| ee.verify_is_valid_for_subject_name(name).is_ok())
}

/// Whether `cert` names `host` (a host name or an IP address, bracketed or not).
pub fn names_host(cert: &CertificateDer, host: &str) -> bool {
    server_name(host).is_ok_and(|name| names(cert, &name))
}

// ---------------------------------------------------------------------------------------------
// The server's side

/// What `serve` speaks TLS with: the certificate chain and its key.
pub struct ServerCert {
    pub chain: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

impl ServerCert {
    /// The SHA-256 fingerprint of the server's own certificate (the first of the chain).
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.chain[0])
    }

    /// A rustls server configuration for HTTP/1.1 with this certificate.
    pub fn server_config(self) -> Result<Arc<rustls::ServerConfig>> {
        let mut config = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()?
            .with_no_client_auth()
            .with_single_cert(self.chain, self.key)
            .context("the certificate and key do not go together")?;
        config.alpn_protocols = vec![b"http/1.1".to_vec()];
        Ok(Arc::new(config))
    }
}

/// A certificate and key you already have (PEM files), for `serve --tls-cert --tls-key`.
pub fn load(cert: &Path, key: &Path) -> Result<ServerCert> {
    let text =
        std::fs::read_to_string(cert).with_context(|| format!("reading {}", cert.display()))?;
    let chain = certs_from_pem(&text).with_context(|| format!("reading {}", cert.display()))?;
    let key = PrivateKeyDer::from_pem_file(key)
        .map_err(|e| anyhow!("reading the key {}: {e}", key.display()))?;
    Ok(ServerCert { chain, key })
}

/// Where `serve --tls` keeps its certificate for the database `db`: `<db>.tls/`.
pub fn dir_for(db: &str) -> PathBuf {
    PathBuf::from(format!("{db}.tls"))
}

/// The certificate stored in `dir`, if there is one.
pub fn stored(dir: &Path) -> Result<Option<ServerCert>> {
    let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
    if !cert.exists() || !key.exists() {
        return Ok(None);
    }
    load(&cert, &key)
        .map(Some)
        .with_context(|| format!("delete {} to make a new certificate", dir.display()))
}

/// What `stored_or_new` did.
#[derive(Debug, PartialEq)]
pub enum Made {
    /// The stored certificate, as it was.
    Kept,
    /// A first certificate, for these names.
    New(Vec<String>),
    /// A new certificate for these names, because the stored one lacked `missing`.
    Replaced {
        names: Vec<String>,
        missing: Vec<String>,
    },
}

/// The certificate `serve --tls` uses: the one stored in `dir`, or a new self-signed one for
/// `names` (`requested`, or else `default`) when there is none or `requested` has names the
/// stored one lacks. Keeps `dir` 0700 and the key in it 0600.
pub fn stored_or_new(
    dir: &Path,
    requested: &[String],
    default: impl FnOnce() -> Vec<String>,
) -> Result<(ServerCert, Made)> {
    for name in requested {
        server_name(name)?;
    }
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    // Also when it was there already, with other permissions.
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("making {} private", dir.display()))?;
    let (names, made) = match stored(dir)? {
        Some(cert) => {
            let missing: Vec<String> = requested
                .iter()
                .filter(|n| !names(&cert.chain[0], &server_name(n).expect("checked above")))
                .cloned()
                .collect();
            if missing.is_empty() {
                let key = dir.join("key.pem");
                std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600))
                    .with_context(|| format!("making {} private", key.display()))?;
                return Ok((cert, Made::Kept));
            }
            let names = requested.to_vec();
            (names.clone(), Made::Replaced { names, missing })
        }
        None => {
            let names = if requested.is_empty() {
                default()
            } else {
                requested.to_vec()
            };
            (names.clone(), Made::New(names))
        }
    };
    let (cert_pem, key_pem) = self_signed(&names)?;
    // The key first: a certificate never sits next to a key that is not its own for longer.
    write_private(&dir.join("key.pem"), &key_pem, 0o600)?;
    write_private(&dir.join("cert.pem"), &cert_pem, 0o644)?;
    let cert =
        stored(dir)?.ok_or_else(|| anyhow!("the new certificate is not in {}", dir.display()))?;
    Ok((cert, made))
}

/// A self-signed certificate (ECDSA P-256) for `names`, as PEM: (certificate, key).
fn self_signed(names: &[String]) -> Result<(String, String)> {
    let bare: Vec<String> = names
        .iter()
        .map(|n| n.trim_start_matches('[').trim_end_matches(']').to_string())
        .collect();
    let mut params = rcgen::CertificateParams::new(bare).context("the certificate's names")?;
    let mut dn = rcgen::DistinguishedName::new();
    dn.push(rcgen::DnType::CommonName, "Relevo");
    params.distinguished_name = dn;
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(VALID_DAYS);
    params.key_usages = vec![rcgen::KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![rcgen::ExtendedKeyUsagePurpose::ServerAuth];
    let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256)?;
    let cert = params.self_signed(&key)?;
    Ok((cert.pem(), key.serialize_pem()))
}

/// Writes `text` to `file` with `mode`, through a new file moved into place.
fn write_private(file: &Path, text: &str, mode: u32) -> Result<()> {
    let tmp = file.with_extension("pem.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&tmp)
        .with_context(|| format!("writing {}", tmp.display()))?;
    f.set_permissions(std::fs::Permissions::from_mode(mode))?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, file).with_context(|| format!("writing {}", file.display()))
}

/// The names a new certificate gets when `--tls-name` gives none: `localhost`, `127.0.0.1`,
/// `::1`, this machine's host name and its network addresses.
pub fn default_names() -> Vec<String> {
    let mut names = vec!["localhost".to_string(), "127.0.0.1".into(), "::1".into()];
    if let Ok(host) = std::fs::read_to_string("/proc/sys/kernel/hostname") {
        let host = host.trim();
        if !host.is_empty() && host != "localhost" && server_name(host).is_ok() {
            names.push(host.to_string());
        }
    }
    for ip in network_addresses() {
        if !names.contains(&ip.to_string()) {
            names.push(ip.to_string());
        }
    }
    names
}

/// This machine's addresses other than loopback and link-local (none if they cannot be read).
pub fn network_addresses() -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = vec![];
    for ip in interface_addresses() {
        let local = match ip {
            IpAddr::V4(v4) => v4.is_loopback() || v4.is_link_local() || v4.is_unspecified(),
            IpAddr::V6(v6) => {
                v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xffc0) == 0xfe80
            }
        };
        if !local && !out.contains(&ip) {
            out.push(ip);
        }
    }
    out
}

/// What `serve --tls` warns about its own certificate, which `--tls-name` did not name: that it
/// has none of this machine's network `addresses`, or that there are none to have (they could
/// not be read). Teammates who connect by address are then refused.
pub fn address_warning(cert: &CertificateDer, addresses: &[IpAddr]) -> Option<String> {
    let fix = "teammates who connect by address are refused; name the addresses and names they \
               use with --tls-name or RELEVO_TLS_NAME (comma-separated)";
    if addresses.is_empty() {
        return Some(format!(
            "found no network address of this machine (under systemd, RestrictAddressFamilies= \
             must allow AF_NETLINK), so the certificate has none: {fix}"
        ));
    }
    if addresses
        .iter()
        .any(|ip| names(cert, &ServerName::IpAddress((*ip).into())))
    {
        return None;
    }
    let list = addresses
        .iter()
        .map(IpAddr::to_string)
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "the certificate has none of this machine's network addresses ({list}): {fix}; that \
         makes a new certificate, and every client runs `relevo connect` again"
    ))
}

/// The addresses of this machine's network interfaces (none if they cannot be read).
fn interface_addresses() -> Vec<IpAddr> {
    let mut out = vec![];
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills `head` with a list that freeifaddrs releases, and nothing here
    // outlives that. Each entry's `ifa_addr`, when not null, points at a socket address of the
    // family its `sa_family` says.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut entry = head;
        while !entry.is_null() {
            let addr = (*entry).ifa_addr;
            if !addr.is_null() {
                match i32::from((*addr).sa_family) {
                    libc::AF_INET => {
                        let a = &*(addr as *const libc::sockaddr_in);
                        out.push(IpAddr::V4(Ipv4Addr::from(u32::from_be(a.sin_addr.s_addr))));
                    }
                    libc::AF_INET6 => {
                        let a = &*(addr as *const libc::sockaddr_in6);
                        out.push(IpAddr::V6(Ipv6Addr::from(a.sin6_addr.s6_addr)));
                    }
                    _ => {}
                }
            }
            entry = (*entry).ifa_next;
        }
        libc::freeifaddrs(head);
    }
    out
}

// ---------------------------------------------------------------------------------------------
// The client's side

/// A rustls client configuration that trusts `pem`'s (first) certificate alone, for the names it
/// has: no system authority, and no other certificate, even one an authority signed.
pub fn pinned(pem: &str) -> Result<rustls::ClientConfig> {
    let cert = certs_from_pem(pem)?.swap_remove(0);
    let provider = provider();
    let verifier = Pinned {
        cert,
        algorithms: provider.signature_verification_algorithms,
    };
    Ok(rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

#[derive(Debug)]
struct Pinned {
    cert: CertificateDer<'static>,
    algorithms: WebPkiSupportedAlgorithms,
}

/// Why a pinned certificate refused the server's. rustls shows it with `Debug`, so that says
/// the same as `Display`.
struct NotPinned;

impl std::fmt::Debug for NotPinned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::fmt::Display for NotPinned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the server's certificate is not the one `relevo connect` saved; if the server has a \
             new certificate, run `relevo connect` again"
        )
    }
}

impl std::error::Error for NotPinned {}

/// Why a pinned certificate, the server's own, was refused: out of its validity dates, as
/// OpenSSL (pi, Hermes) refuses it too.
struct OutOfDate(&'static str);

impl std::fmt::Debug for OutOfDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, f)
    }
}

impl std::fmt::Display for OutOfDate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the server's certificate, the one `relevo connect` saved, {}: the server needs a \
             new certificate; then run `relevo connect` again",
            self.0
        )
    }
}

impl std::error::Error for OutOfDate {}

/// A certificate's validity (DER): its notBefore and notAfter, in Unix seconds.
fn validity(cert: &[u8]) -> Option<(i64, i64)> {
    /// The first DER element of `d`: its tag, its contents, and what follows it.
    fn element(d: &[u8]) -> Option<(u8, &[u8], &[u8])> {
        let (&tag, d) = d.split_first()?;
        let (&len, mut d) = d.split_first()?;
        let len = if len < 0x80 {
            usize::from(len)
        } else {
            let n = usize::from(len & 0x7f);
            if n == 0 || n > 4 || d.len() < n {
                return None;
            }
            let (bytes, rest) = d.split_at(n);
            d = rest;
            bytes.iter().fold(0, |v, &b| (v << 8) | usize::from(b))
        };
        (d.len() >= len).then(|| (tag, &d[..len], &d[len..]))
    }
    /// UTCTime (0x17, `YYMMDDHHMMSSZ`) or GeneralizedTime (0x18, `YYYYMMDDHHMMSSZ`).
    fn moment(tag: u8, text: &[u8]) -> Option<i64> {
        let text = std::str::from_utf8(text).ok()?.strip_suffix('Z')?;
        let (year, rest) = match tag {
            0x17 => {
                let yy: i32 = text.get(..2)?.parse().ok()?;
                (if yy >= 50 { 1900 + yy } else { 2000 + yy }, &text[2..])
            }
            0x18 => (text.get(..4)?.parse().ok()?, &text[4..]),
            _ => return None,
        };
        if rest.len() != 10 || !rest.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        let two = |i: usize| rest[i..i + 2].parse::<u8>().ok();
        let month = time::Month::try_from(two(0)?).ok()?;
        let date = time::Date::from_calendar_date(year, month, two(2)?).ok()?;
        let at = time::Time::from_hms(two(4)?, two(6)?, two(8)?).ok()?;
        Some(
            time::PrimitiveDateTime::new(date, at)
                .assume_utc()
                .unix_timestamp(),
        )
    }
    let (0x30, cert, _) = element(cert)? else {
        return None;
    };
    let (0x30, tbs, _) = element(cert)? else {
        return None;
    };
    let (tag, _, mut rest) = element(tbs)?;
    // [0] version, then the serial number.
    if tag == 0xa0 {
        (_, _, rest) = element(rest)?;
    }
    let (_, _, rest) = element(rest)?; // the signature algorithm
    let (_, _, rest) = element(rest)?; // the issuer
    let (0x30, dates, _) = element(rest)? else {
        return None;
    };
    let (from_tag, from, dates) = element(dates)?;
    let (until_tag, until, _) = element(dates)?;
    Some((moment(from_tag, from)?, moment(until_tag, until)?))
}

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() != self.cert.as_ref() {
            return Err(rustls::Error::InvalidCertificate(CertificateError::Other(
                rustls::OtherError(Arc::new(NotPinned)),
            )));
        }
        if !names(end_entity, server_name) {
            return Err(rustls::Error::InvalidCertificate(
                CertificateError::NotValidForName,
            ));
        }
        let out_of_date = |why| {
            rustls::Error::InvalidCertificate(CertificateError::Other(rustls::OtherError(
                Arc::new(OutOfDate(why)),
            )))
        };
        let now = i64::try_from(now.as_secs()).unwrap_or(i64::MAX);
        match validity(end_entity) {
            None => Err(rustls::Error::InvalidCertificate(
                CertificateError::BadEncoding,
            )),
            Some((from, _)) if now < from => Err(out_of_date("is not valid yet")),
            Some((_, until)) if now > until => Err(out_of_date("has expired")),
            Some(_) => Ok(ServerCertVerified::assertion()),
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

/// What a TLS handshake showed of a server's certificate.
pub struct Seen {
    /// The server's own certificate.
    pub cert: CertificateDer<'static>,
    /// Whether this machine's trusted authorities accept it for the host.
    pub trusted: bool,
    /// Whether it names the host the URL has.
    pub names_host: bool,
}

/// Shakes hands with the `https://` server of `url` and sends nothing: what certificate it has,
/// and whether the system trusts it.
pub async fn look(url: &str) -> Result<Seen> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("not a URL: {url}"))?;
    let port = parsed.port_or_known_default().unwrap_or(443);
    let host = parsed
        .host_str()
        .ok_or_else(|| anyhow!("no host in {url}"))?;
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    let name = server_name(bare)?;
    let tcp = match bare.parse::<IpAddr>() {
        Ok(ip) => {
            tokio::time::timeout(LOOK_TIMEOUT, tokio::net::TcpStream::connect((ip, port))).await
        }
        Err(_) => {
            tokio::time::timeout(LOOK_TIMEOUT, tokio::net::TcpStream::connect((bare, port))).await
        }
    };
    let reach = |e: String| anyhow!("could not reach the Relevo server: {e}");
    let tcp = tcp
        .map_err(|_| reach("timed out".into()))?
        .map_err(|e| reach(e.to_string()))?;
    let provider = provider();
    let look = Arc::new(Look {
        system: rustls_platform_verifier::Verifier::new(provider.clone()).ok(),
        algorithms: provider.signature_verification_algorithms,
        seen: Mutex::new(None),
    });
    let config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(look.clone())
        .with_no_client_auth();
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let shake = tokio::time::timeout(LOOK_TIMEOUT, connector.connect(name.clone(), tcp)).await;
    let mut stream = shake
        .map_err(|_| reach("the TLS handshake timed out".into()))?
        .map_err(|e| reach(format!("TLS handshake: {e}")))?;
    let _ = tokio::io::AsyncWriteExt::shutdown(&mut stream).await;
    let (cert, trusted) = look
        .seen
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .take()
        .ok_or_else(|| reach("the server showed no certificate".into()))?;
    let names_host = names(&cert, &name);
    Ok(Seen {
        cert,
        trusted,
        names_host,
    })
}

const LOOK_TIMEOUT: Duration = Duration::from_secs(10);

/// Accepts any certificate, after noting it and whether the system's verifier accepts it. Only
/// for looking: nothing is sent over a connection it accepted.
#[derive(Debug)]
struct Look {
    system: Option<rustls_platform_verifier::Verifier>,
    algorithms: WebPkiSupportedAlgorithms,
    seen: Mutex<Option<(CertificateDer<'static>, bool)>>,
}

impl ServerCertVerifier for Look {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let trusted = self.system.as_ref().is_some_and(|v| {
            v.verify_server_cert(end_entity, intermediates, server_name, ocsp, now)
                .is_ok()
        });
        *self.seen.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((end_entity.clone().into_owned(), trusted));
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algorithms.supported_schemes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn tempdir() -> PathBuf {
        let dir = std::env::temp_dir().join(crate::db::random_id("relevo-tls-"));
        std::fs::create_dir(&dir).unwrap();
        dir
    }

    fn mode(p: &Path) -> u32 {
        std::fs::metadata(p).unwrap().permissions().mode() & 0o777
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn named(cert: &ServerCert, name: &str) -> bool {
        names(&cert.chain[0], &server_name(name).unwrap())
    }

    #[test]
    fn a_new_certificate_is_kept_privately_and_reused() {
        let base = tempdir();
        let dir = base.join("relevo.db.tls");
        let default = || strings(&["localhost", "127.0.0.1", "::1"]);
        let (first, made) = stored_or_new(&dir, &[], default).unwrap();
        assert_eq!(made, Made::New(default()));
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("key.pem")), 0o600);
        for name in ["localhost", "127.0.0.1", "::1", "[::1]"] {
            assert!(named(&first, name), "{name}");
        }
        assert!(!named(&first, "relevo.office"));

        // Reused, and made private again if someone opened it up.
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(dir.join("key.pem"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        let (again, made) = stored_or_new(&dir, &[], || panic!("no new names")).unwrap();
        assert_eq!(made, Made::Kept);
        assert_eq!(again.fingerprint(), first.fingerprint());
        assert_eq!((mode(&dir), mode(&dir.join("key.pem"))), (0o700, 0o600));

        // Names it already has change nothing.
        let (again, made) = stored_or_new(&dir, &strings(&["127.0.0.1"]), default).unwrap();
        assert_eq!(made, Made::Kept);
        assert_eq!(again.fingerprint(), first.fingerprint());

        // A name it lacks makes a new one, for the names asked.
        let asked = strings(&["relevo.office", "192.168.10.51"]);
        let (new, made) = stored_or_new(&dir, &asked, default).unwrap();
        assert_eq!(
            made,
            Made::Replaced {
                names: asked.clone(),
                missing: asked.clone()
            }
        );
        assert_ne!(new.fingerprint(), first.fingerprint());
        assert!(named(&new, "relevo.office") && named(&new, "192.168.10.51"));
        assert_eq!(mode(&dir.join("key.pem")), 0o600);
        // And it is what is stored now.
        let stored = stored(&dir).unwrap().unwrap();
        assert_eq!(stored.fingerprint(), new.fingerprint());
        stored.server_config().unwrap();

        assert!(stored_or_new(&dir, &strings(&["not a name"]), default).is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }

    /// A self-signed certificate for `localhost`, valid from `from` to `until` (Unix seconds).
    fn dated(from: i64, until: i64) -> CertificateDer<'static> {
        let mut params = rcgen::CertificateParams::new(strings(&["localhost"])).unwrap();
        params.not_before = time::OffsetDateTime::from_unix_timestamp(from).unwrap();
        params.not_after = time::OffsetDateTime::from_unix_timestamp(until).unwrap();
        let key = rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).unwrap();
        params.self_signed(&key).unwrap().der().clone()
    }

    #[test]
    fn a_pinned_certificate_out_of_its_dates_is_refused() {
        let day = 86_400;
        let now = db::now_unix();
        let check = |cert: &CertificateDer<'static>, at: i64| {
            let pinned = Pinned {
                cert: cert.clone(),
                algorithms: provider().signature_verification_algorithms,
            };
            let name = server_name("localhost").unwrap();
            let at = UnixTime::since_unix_epoch(Duration::from_secs(at as u64));
            pinned
                .verify_server_cert(cert, &[], &name, &[], at)
                .map_err(|e| e.to_string())
        };
        let current = dated(now - day, now + day);
        assert!(check(&current, now).is_ok());
        // OpenSSL (pi, Hermes) refuses it too.
        let expired = dated(now - 3 * day, now - day);
        let err = check(&expired, now).unwrap_err();
        assert!(
            err.contains("expired") && err.contains("relevo connect"),
            "{err}"
        );
        let early = dated(now + day, now + 3 * day);
        let err = check(&early, now).unwrap_err();
        assert!(err.contains("not valid yet"), "{err}");
        // Dates past 2049 are written another way (GeneralizedTime).
        let far = 2_600_000_000; // 2052
        let long = dated(now - day, far);
        assert_eq!(validity(&long), Some((now - day, far)));
        assert!(check(&long, far - 1).is_ok() && check(&long, far + 1).is_err());
    }

    #[test]
    fn fingerprints_compare_loosely() {
        let fp = "AB:CD:01";
        assert!(same_fingerprint("AB:CD:01", fp));
        assert!(same_fingerprint("abcd01", fp));
        assert!(same_fingerprint(" ab cd 01 ", fp));
        assert!(!same_fingerprint("ABCD02", fp));
        assert!(!same_fingerprint("", ""));
    }

    #[test]
    fn pem_round_trips() {
        let (cert, _) = self_signed(&strings(&["localhost"])).unwrap();
        let der = certs_from_pem(&cert).unwrap().swap_remove(0);
        assert_eq!(certs_from_pem(&pem(&der)).unwrap()[0], der);
        assert!(certs_from_pem("nothing").is_err());
    }

    #[test]
    fn a_certificate_without_the_machines_addresses_is_warned_about() {
        let lan: IpAddr = "192.168.1.50".parse().unwrap();
        let vpn: IpAddr = "100.69.148.45".parse().unwrap();
        let der = |names: &[&str]| {
            let (cert, _) = self_signed(&strings(names)).unwrap();
            certs_from_pem(&cert).unwrap().swap_remove(0)
        };
        let local = der(&["localhost", "127.0.0.1", "::1", "box"]);
        // None could be read (getifaddrs failed): said, with why it may have.
        let none = address_warning(&local, &[]).unwrap();
        assert!(
            none.contains("AF_NETLINK") && none.contains("RELEVO_TLS_NAME"),
            "{none}"
        );
        // There are some, and it has none of them.
        let lacks = address_warning(&local, &[lan, vpn]).unwrap();
        assert!(lacks.contains("192.168.1.50, 100.69.148.45"), "{lacks}");
        assert!(lacks.contains("--tls-name"), "{lacks}");
        // It has one of them: nothing to say.
        let with_lan = der(&["localhost", "192.168.1.50"]);
        assert_eq!(address_warning(&with_lan, &[lan, vpn]), None);
        // The machine's own, as found here, are the ones default_names gives it.
        let found = network_addresses();
        let made = der(&default_names()
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>());
        assert_eq!(address_warning(&made, &found).is_some(), found.is_empty());
    }

    #[test]
    fn default_names_have_loopback_and_no_link_local() {
        let names = default_names();
        assert_eq!(names[..3], strings(&["localhost", "127.0.0.1", "::1"]));
        assert!(
            !names
                .iter()
                .any(|n| n.starts_with("fe80") || n.starts_with("169.254"))
        );
        for n in &names {
            server_name(n).unwrap();
        }
    }
}
