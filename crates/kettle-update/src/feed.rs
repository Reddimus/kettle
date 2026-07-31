use std::io::Read;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
use semver::Version;
use serde::{Deserialize, Serialize};

use crate::{MANIFEST_URL, SIGNATURE_URL, SIGNING_CONTEXT, UPDATE_PUBLIC_KEY, current_target};

const MAX_MANIFEST_BYTES: usize = 128 * 1024;
const MAX_SIGNATURE_BYTES: usize = 1024;
pub(crate) const MAX_ARTIFACT_BYTES: u64 = 256 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const IO_TIMEOUT: Duration = Duration::from_secs(10);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(20);
const RELEASE_DOWNLOAD_PREFIX: &str = "https://github.com/Reddimus/kettle/releases/download";
pub(crate) const MAX_MANIFEST_AGE: Duration = Duration::from_secs(90 * 24 * 60 * 60);
pub(crate) const MAX_MANIFEST_FUTURE_SKEW: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("update request failed: {0}")]
    Request(String),
    #[error("update response exceeded the {0}-byte safety limit")]
    ResponseTooLarge(usize),
    #[error("the update manifest signature is malformed")]
    MalformedSignature,
    #[error("the update manifest signature is invalid")]
    InvalidSignature,
    #[error("the update manifest is malformed: {0}")]
    MalformedManifest(String),
    #[error("the signed update manifest is stale: {0}")]
    StaleManifest(String),
    #[error("the update was not derived from authenticated release metadata")]
    UnauthenticatedRelease,
    #[error("refusing to replace installed Kettle {installed} with {candidate}")]
    Rollback {
        candidate: Version,
        installed: Version,
    },
    #[error("unsupported update manifest schema {0}")]
    UnsupportedSchema(u32),
    #[error("the signed manifest has no artifact for {0}")]
    MissingTarget(String),
    #[error("self-update is not supported on this platform")]
    UnsupportedPlatform,
    #[error("invalid current kettle version {0:?}")]
    InvalidCurrentVersion(String),
    #[error("this kettle executable is not owned by the official installer: {0}")]
    UnmanagedInstall(String),
    #[error("another kettle update is already running")]
    UpdateLocked,
    #[error(
        "downloaded artifact did not match its signed size (expected {expected}, got {actual})"
    )]
    SizeMismatch { expected: u64, actual: u64 },
    #[error("downloaded artifact failed SHA-256 verification")]
    HashMismatch,
    #[error("unsafe or unsupported archive entry: {0}")]
    UnsafeArchive(String),
    #[error("the release archive is missing required file {0}")]
    MissingArchiveFile(String),
    #[error("update transaction failed: {0}")]
    Transaction(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
}

/// Signed stable-channel metadata. Unknown/new layouts require a schema bump so
/// an old updater fails closed instead of guessing at security-sensitive fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub product: String,
    pub channel: String,
    pub version: String,
    pub tag: String,
    pub published_at: String,
    pub assets: Vec<ManifestAsset>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ManifestAsset {
    pub target: String,
    pub name: String,
    pub size: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AvailableUpdate {
    pub version: Version,
    pub tag: String,
    pub release_url: String,
    pub download_url: Option<String>,
    pub asset: Option<ManifestAsset>,
    pub(crate) signed_manifest: Option<SignedManifest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SignedManifest {
    pub(crate) manifest: String,
    pub(crate) signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckOutcome {
    UpdateAvailable(AvailableUpdate),
    UpToDate { latest: Version },
}

/// Production client. Its source and trust root deliberately have no environment
/// override; tests exercise custom feeds through a private constructor below.
pub struct FeedClient {
    manifest_url: String,
    signature_url: String,
    download_prefix: String,
    public_key: [u8; 32],
    agent: ureq::Agent,
}

impl Default for FeedClient {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedClient {
    pub fn new() -> Self {
        Self::from_parts(
            MANIFEST_URL.to_string(),
            SIGNATURE_URL.to_string(),
            RELEASE_DOWNLOAD_PREFIX.to_string(),
            UPDATE_PUBLIC_KEY,
        )
    }

    fn from_parts(
        manifest_url: String,
        signature_url: String,
        download_prefix: String,
        public_key: [u8; 32],
    ) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout_read(IO_TIMEOUT)
            .timeout_write(IO_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .redirects(5)
            .build();
        Self {
            manifest_url,
            signature_url,
            download_prefix,
            public_key,
            agent,
        }
    }

    pub fn fetch_manifest(&self) -> Result<Manifest, UpdateError> {
        self.fetch_signed_manifest().map(|(manifest, _)| manifest)
    }

    fn fetch_signed_manifest(&self) -> Result<(Manifest, SignedManifest), UpdateError> {
        let manifest = self.get_limited(&self.manifest_url, MAX_MANIFEST_BYTES)?;
        let signature = self.get_limited(&self.signature_url, MAX_SIGNATURE_BYTES)?;
        let verified = verify_manifest(&manifest, &signature, &self.public_key)?;
        let signed = SignedManifest {
            manifest: String::from_utf8(manifest)
                .map_err(|error| UpdateError::MalformedManifest(error.to_string()))?,
            signature: std::str::from_utf8(&signature)
                .map_err(|_| UpdateError::MalformedSignature)?
                .trim()
                .to_string(),
        };
        Ok((verified, signed))
    }

    pub fn check(&self, current: &str) -> Result<CheckOutcome, UpdateError> {
        self.check_at(current, SystemTime::now())
    }

    fn check_at(&self, current: &str, now: SystemTime) -> Result<CheckOutcome, UpdateError> {
        let (manifest, signed) = self.fetch_signed_manifest()?;
        validate_manifest_freshness(&manifest, now)?;
        evaluate_manifest(
            manifest,
            current,
            current_target(),
            &self.download_prefix,
            Some(signed),
        )
    }

    #[cfg(any(windows, target_os = "linux"))]
    pub(crate) fn download_to<W: std::io::Write>(
        &self,
        update: &AvailableUpdate,
        mut output: W,
    ) -> Result<(), UpdateError> {
        let asset = update
            .asset
            .as_ref()
            .ok_or(UpdateError::UnsupportedPlatform)?;
        let download_url = update
            .download_url
            .as_deref()
            .ok_or(UpdateError::UnsupportedPlatform)?;
        if asset.size == 0 || asset.size > MAX_ARTIFACT_BYTES {
            return Err(UpdateError::MalformedManifest(
                "artifact size is outside the accepted range".to_string(),
            ));
        }
        let response = self
            .agent
            .get(download_url)
            .set("User-Agent", user_agent())
            .set("Accept", "application/octet-stream")
            .call()
            .map_err(|e| UpdateError::Request(e.to_string()))?;
        if let Some(length) = response.header("Content-Length")
            && let Ok(length) = length.parse::<u64>()
            && length != asset.size
        {
            return Err(UpdateError::SizeMismatch {
                expected: asset.size,
                actual: length,
            });
        }
        let mut reader = response.into_reader().take(asset.size + 1);
        let actual = std::io::copy(&mut reader, &mut output)?;
        if actual != asset.size {
            return Err(UpdateError::SizeMismatch {
                expected: asset.size,
                actual,
            });
        }
        Ok(())
    }

    /// Download one Linux update into the single bounded buffer that both the
    /// digest verifier and archive extractor consume. Keeping verified bytes
    /// off disk closes the remaining same-user in-place-overwrite window
    /// between two reads of a temporary archive inode.
    #[cfg(target_os = "linux")]
    pub(crate) fn download_bytes(&self, update: &AvailableUpdate) -> Result<Vec<u8>, UpdateError> {
        let asset = update
            .asset
            .as_ref()
            .ok_or(UpdateError::UnsupportedPlatform)?;
        if asset.size == 0 || asset.size > MAX_ARTIFACT_BYTES {
            return Err(UpdateError::MalformedManifest(
                "artifact size is outside the accepted range".to_string(),
            ));
        }
        let capacity = usize::try_from(asset.size).map_err(|_| {
            UpdateError::MalformedManifest(
                "artifact size does not fit this platform's address space".to_string(),
            )
        })?;
        let reserve = capacity.checked_add(1).ok_or_else(|| {
            UpdateError::MalformedManifest(
                "artifact size does not fit this platform's address space".to_string(),
            )
        })?;
        let mut bytes = Vec::new();
        // One sentinel byte lets `download_to` detect an overlong body without
        // growing the Vec beyond this explicit signed-size-derived bound.
        bytes.try_reserve_exact(reserve).map_err(|error| {
            UpdateError::Io(std::io::Error::other(format!(
                "could not reserve the signed {capacity}-byte update buffer plus sentinel: {error}"
            )))
        })?;
        self.download_to(update, &mut bytes)?;
        Ok(bytes)
    }

    fn get_limited(&self, url: &str, limit: usize) -> Result<Vec<u8>, UpdateError> {
        let response = self
            .agent
            .get(url)
            .set("User-Agent", user_agent())
            .set("Accept", "application/octet-stream")
            .call()
            .map_err(|e| UpdateError::Request(e.to_string()))?;
        if let Some(length) = response.header("Content-Length")
            && let Ok(length) = length.parse::<usize>()
            && length > limit
        {
            return Err(UpdateError::ResponseTooLarge(limit));
        }
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(limit as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > limit {
            return Err(UpdateError::ResponseTooLarge(limit));
        }
        Ok(bytes)
    }

    #[cfg(test)]
    fn for_test(base: &str, public_key: [u8; 32]) -> Self {
        let base = base.trim_end_matches('/');
        Self::from_parts(
            format!("{base}/manifest"),
            format!("{base}/signature"),
            format!("{base}/download"),
            public_key,
        )
    }
}

fn evaluate_manifest(
    manifest: Manifest,
    current: &str,
    target: Option<&str>,
    download_prefix: &str,
    signed_manifest: Option<SignedManifest>,
) -> Result<CheckOutcome, UpdateError> {
    let latest = Version::parse(&manifest.version)
        .map_err(|e| UpdateError::MalformedManifest(format!("invalid version: {e}")))?;
    let current = Version::parse(current)
        .map_err(|_| UpdateError::InvalidCurrentVersion(current.to_string()))?;
    if latest <= current {
        return Ok(CheckOutcome::UpToDate { latest });
    }

    let asset = target
        .map(|target| {
            manifest
                .assets
                .iter()
                .find(|asset| asset.target == target)
                .cloned()
                .ok_or_else(|| UpdateError::MissingTarget(target.to_string()))
        })
        .transpose()?;
    let prefix = download_prefix.trim_end_matches('/');
    let download_url = asset
        .as_ref()
        .map(|asset| format!("{}/{}/{}", prefix, manifest.tag, asset.name));
    Ok(CheckOutcome::UpdateAvailable(AvailableUpdate {
        version: latest,
        release_url: format!(
            "https://github.com/Reddimus/kettle/releases/tag/{}",
            manifest.tag
        ),
        download_url,
        tag: manifest.tag,
        asset,
        signed_manifest,
    }))
}

pub(crate) fn reverify_available_update(
    update: &AvailableUpdate,
    public_key: &[u8; 32],
    now: SystemTime,
) -> Result<Manifest, UpdateError> {
    let signed = update
        .signed_manifest
        .as_ref()
        .ok_or(UpdateError::UnauthenticatedRelease)?;
    let manifest = verify_manifest(
        signed.manifest.as_bytes(),
        signed.signature.as_bytes(),
        public_key,
    )?;
    validate_manifest_freshness(&manifest, now)?;
    let version = Version::parse(&manifest.version)
        .map_err(|error| UpdateError::MalformedManifest(error.to_string()))?;
    let target = current_target().ok_or(UpdateError::UnsupportedPlatform)?;
    let asset = manifest
        .assets
        .iter()
        .find(|asset| asset.target == target)
        .ok_or_else(|| UpdateError::MissingTarget(target.to_string()))?;
    if version != update.version
        || manifest.tag != update.tag
        || update.asset.as_ref() != Some(asset)
    {
        return Err(UpdateError::MalformedManifest(
            "selected update does not match its signed manifest".into(),
        ));
    }
    Ok(manifest)
}

pub(crate) fn require_strict_upgrade(
    candidate: &Version,
    installed: &Version,
) -> Result<(), UpdateError> {
    if candidate <= installed {
        return Err(UpdateError::Rollback {
            candidate: candidate.clone(),
            installed: installed.clone(),
        });
    }
    Ok(())
}

fn user_agent() -> &'static str {
    concat!(
        "kettle/",
        env!("CARGO_PKG_VERSION"),
        " (+https://github.com/Reddimus/kettle)"
    )
}

pub fn verify_manifest(
    manifest_bytes: &[u8],
    signature_text: &[u8],
    public_key: &[u8; 32],
) -> Result<Manifest, UpdateError> {
    if manifest_bytes.is_empty() || manifest_bytes.len() > MAX_MANIFEST_BYTES {
        return Err(UpdateError::ResponseTooLarge(MAX_MANIFEST_BYTES));
    }
    let signature_text = std::str::from_utf8(signature_text)
        .map_err(|_| UpdateError::MalformedSignature)?
        .trim();
    let signature_bytes = base64::engine::general_purpose::STANDARD
        .decode(signature_text)
        .map_err(|_| UpdateError::MalformedSignature)?;
    let signature =
        Signature::from_slice(&signature_bytes).map_err(|_| UpdateError::MalformedSignature)?;
    let key = VerifyingKey::from_bytes(public_key).map_err(|_| UpdateError::InvalidSignature)?;
    let mut signed = Vec::with_capacity(SIGNING_CONTEXT.len() + manifest_bytes.len());
    signed.extend_from_slice(SIGNING_CONTEXT);
    signed.extend_from_slice(manifest_bytes);
    key.verify(&signed, &signature)
        .map_err(|_| UpdateError::InvalidSignature)?;

    let manifest: Manifest = serde_json::from_slice(manifest_bytes)
        .map_err(|e| UpdateError::MalformedManifest(e.to_string()))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &Manifest) -> Result<(), UpdateError> {
    if manifest.schema != 1 {
        return Err(UpdateError::UnsupportedSchema(manifest.schema));
    }
    if manifest.product != "kettle" || manifest.channel != "stable" {
        return Err(UpdateError::MalformedManifest(
            "unexpected product or channel".to_string(),
        ));
    }
    let version = Version::parse(&manifest.version)
        .map_err(|e| UpdateError::MalformedManifest(format!("invalid version: {e}")))?;
    if !version.pre.is_empty() || !version.build.is_empty() || manifest.tag != format!("v{version}")
    {
        return Err(UpdateError::MalformedManifest(
            "stable version and tag do not agree".to_string(),
        ));
    }
    if parse_rfc3339_seconds(&manifest.published_at).is_none() || manifest.assets.is_empty() {
        return Err(UpdateError::MalformedManifest(
            "published_at must be an RFC 3339 timestamp and assets are required".to_string(),
        ));
    }
    let mut targets = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    for asset in &manifest.assets {
        if !targets.insert(asset.target.as_str()) || !names.insert(asset.name.as_str()) {
            return Err(UpdateError::MalformedManifest(
                "asset targets and names must be unique".to_string(),
            ));
        }
        if asset.size == 0 || asset.size > MAX_ARTIFACT_BYTES {
            return Err(UpdateError::MalformedManifest(
                "artifact size is outside the accepted range".to_string(),
            ));
        }
        if asset.name.contains('/')
            || asset.name.contains('\\')
            || asset.name.contains("..")
            || !asset.name.starts_with("kettle-")
        {
            return Err(UpdateError::MalformedManifest(
                "artifact name is unsafe".to_string(),
            ));
        }
        if asset.sha256.len() != 64
            || !asset
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(UpdateError::MalformedManifest(
                "artifact SHA-256 must be lowercase hexadecimal".to_string(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_manifest_freshness(
    manifest: &Manifest,
    now: SystemTime,
) -> Result<(), UpdateError> {
    let published = parse_rfc3339_seconds(&manifest.published_at).ok_or_else(|| {
        UpdateError::MalformedManifest("published_at must be an RFC 3339 timestamp".into())
    })?;
    let now = now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| UpdateError::StaleManifest("system clock predates Unix epoch".into()))?
        .as_secs();
    let published = u64::try_from(published)
        .map_err(|_| UpdateError::MalformedManifest("published_at predates Unix epoch".into()))?;
    if published > now.saturating_add(MAX_MANIFEST_FUTURE_SKEW.as_secs()) {
        return Err(UpdateError::StaleManifest(format!(
            "published_at {} is too far in the future",
            manifest.published_at
        )));
    }
    if now.saturating_sub(published) > MAX_MANIFEST_AGE.as_secs() {
        return Err(UpdateError::StaleManifest(format!(
            "published_at {} is older than {} days",
            manifest.published_at,
            MAX_MANIFEST_AGE.as_secs() / 86_400
        )));
    }
    Ok(())
}

fn parse_rfc3339_seconds(value: &str) -> Option<i64> {
    let (date, time_and_zone) = value.split_once('T')?;
    if date.len() != 10
        || date.as_bytes().get(4) != Some(&b'-')
        || date.as_bytes().get(7) != Some(&b'-')
    {
        return None;
    }
    let year = date.get(0..4)?.parse::<i64>().ok()?;
    let month = date.get(5..7)?.parse::<u32>().ok()?;
    let day = date.get(8..10)?.parse::<u32>().ok()?;
    if !(1970..=9999).contains(&year) || !(1..=12).contains(&month) {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [
        31,
        28 + u32::from(leap),
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if day == 0 || day > month_days[usize::try_from(month - 1).ok()?] {
        return None;
    }

    let (time, offset_seconds) = if let Some(time) = time_and_zone.strip_suffix('Z') {
        (time, 0_i64)
    } else {
        let zone_at = time_and_zone.rfind(['+', '-'])?;
        let (time, zone) = time_and_zone.split_at(zone_at);
        if zone.len() != 6 || zone.as_bytes().get(3) != Some(&b':') {
            return None;
        }
        let hours = zone.get(1..3)?.parse::<i64>().ok()?;
        let minutes = zone.get(4..6)?.parse::<i64>().ok()?;
        if hours > 23 || minutes > 59 {
            return None;
        }
        let magnitude = hours
            .checked_mul(3600)?
            .checked_add(minutes.checked_mul(60)?)?;
        let offset = if zone.starts_with('+') {
            magnitude
        } else if zone.starts_with('-') {
            -magnitude
        } else {
            return None;
        };
        (time, offset)
    };
    let clock = time.split_once('.').map_or(time, |(clock, fraction)| {
        if fraction.is_empty()
            || fraction.len() > 9
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            ""
        } else {
            clock
        }
    });
    if clock.len() != 8
        || clock.as_bytes().get(2) != Some(&b':')
        || clock.as_bytes().get(5) != Some(&b':')
    {
        return None;
    }
    let hour = clock.get(0..2)?.parse::<i64>().ok()?;
    let minute = clock.get(3..5)?.parse::<i64>().ok()?;
    let second = clock.get(6..8)?.parse::<i64>().ok()?;
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }

    let adjusted_year = year - i64::from(month <= 2);
    let era = adjusted_year.div_euclid(400);
    let year_of_era = adjusted_year - era * 400;
    let shifted_month = i64::from(month) + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    days.checked_mul(86_400)?
        .checked_add(hour.checked_mul(3600)?)?
        .checked_add(minute.checked_mul(60)?)?
        .checked_add(second)?
        .checked_sub(offset_seconds)
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::Arc;

    use base64::Engine as _;
    use ed25519_dalek::{Signer as _, SigningKey};

    use super::*;

    const TEST_SECRET: [u8; 32] = [7; 32];

    fn manifest() -> Manifest {
        Manifest {
            schema: 1,
            product: "kettle".into(),
            channel: "stable".into(),
            version: "99.0.0".into(),
            tag: "v99.0.0".into(),
            published_at: "2026-07-11T00:00:00Z".into(),
            assets: vec![ManifestAsset {
                target: current_target().unwrap_or("unsupported-test").into(),
                name: "kettle-test.tar.gz".into(),
                size: 4,
                sha256: "a".repeat(64),
            }],
        }
    }

    fn signed_manifest() -> (Vec<u8>, Vec<u8>, [u8; 32]) {
        let bytes = serde_json::to_vec(&manifest()).unwrap();
        let key = SigningKey::from_bytes(&TEST_SECRET);
        let mut payload = SIGNING_CONTEXT.to_vec();
        payload.extend_from_slice(&bytes);
        let signature = key.sign(&payload);
        let encoded = base64::engine::general_purpose::STANDARD
            .encode(signature.to_bytes())
            .into_bytes();
        (bytes, encoded, key.verifying_key().to_bytes())
    }

    #[test]
    fn authentic_manifest_verifies_before_parsing() {
        let (bytes, signature, public) = signed_manifest();
        assert_eq!(
            verify_manifest(&bytes, &signature, &public).unwrap(),
            manifest()
        );

        let mut tampered = bytes;
        tampered[0] ^= 1;
        assert!(matches!(
            verify_manifest(&tampered, &signature, &public),
            Err(UpdateError::InvalidSignature)
        ));
    }

    #[test]
    fn manifest_validation_rejects_ambiguous_assets() {
        let mut bad = manifest();
        bad.assets.push(bad.assets[0].clone());
        assert!(validate_manifest(&bad).is_err());
        bad = manifest();
        bad.assets[0].name = "../kettle.exe".into();
        assert!(validate_manifest(&bad).is_err());
        bad = manifest();
        bad.tag = "v99.0.1".into();
        assert!(validate_manifest(&bad).is_err());
    }

    #[test]
    fn unsupported_platform_still_discovers_a_signed_release() {
        let outcome =
            evaluate_manifest(manifest(), "1.0.0", None, "https://example.invalid", None).unwrap();
        let CheckOutcome::UpdateAvailable(update) = outcome else {
            panic!("expected newer release");
        };
        assert!(update.asset.is_none());
        assert!(update.download_url.is_none());
    }

    #[test]
    fn signed_feed_check_is_hermetic() {
        let (manifest_body, signature, public) = signed_manifest();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let manifest_bytes = Arc::new(manifest_body);
        let signature = Arc::new(signature);
        let server = std::thread::spawn(move || {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 2048];
                let n = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..n]);
                let body = if request.starts_with("GET /manifest ") {
                    manifest_bytes.as_slice()
                } else {
                    signature.as_slice()
                };
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(body).unwrap();
            }
        });
        let client = FeedClient::for_test(&format!("http://{addr}"), public);
        let published = parse_rfc3339_seconds(&manifest().published_at).unwrap();
        let now = UNIX_EPOCH + Duration::from_secs(u64::try_from(published).unwrap());
        let outcome = client.check_at("1.0.0", now).unwrap();
        assert!(matches!(outcome, CheckOutcome::UpdateAvailable(_)));
        server.join().unwrap();
    }

    #[test]
    fn signed_manifest_timestamp_is_strict_and_bounded() {
        let cases = [
            ("1970-01-01T00:00:00Z", 0),
            ("2000-02-29T12:34:56.123456789+02:30", 951_818_696),
            ("2026-07-11T00:00:00-07:00", 1_783_753_200),
        ];
        for (timestamp, expected) in cases {
            assert_eq!(
                parse_rfc3339_seconds(timestamp),
                Some(expected),
                "{timestamp}"
            );
        }
        for invalid in [
            "",
            "2026-02-29T00:00:00Z",
            "2026-07-11 00:00:00Z",
            "2026-07-11T24:00:00Z",
            "2026-07-11T00:00:00",
            "2026-07-11T00:00:00.0000000000Z",
        ] {
            assert_eq!(parse_rfc3339_seconds(invalid), None, "accepted {invalid}");
        }
    }

    #[test]
    fn signed_manifest_freshness_rejects_expired_and_future_metadata() {
        let mut candidate = manifest();
        candidate.published_at = "2026-07-11T00:00:00Z".into();
        let published =
            u64::try_from(parse_rfc3339_seconds(&candidate.published_at).unwrap()).unwrap();
        validate_manifest_freshness(&candidate, UNIX_EPOCH + Duration::from_secs(published))
            .unwrap();

        let stale = UNIX_EPOCH + Duration::from_secs(published + MAX_MANIFEST_AGE.as_secs() + 1);
        assert!(matches!(
            validate_manifest_freshness(&candidate, stale),
            Err(UpdateError::StaleManifest(_))
        ));

        let before_publication =
            UNIX_EPOCH + Duration::from_secs(published - MAX_MANIFEST_FUTURE_SKEW.as_secs() - 1);
        assert!(matches!(
            validate_manifest_freshness(&candidate, before_publication),
            Err(UpdateError::StaleManifest(_))
        ));
    }

    #[test]
    fn pending_update_versions_must_be_strictly_newer() {
        let installed = Version::new(2, 45, 0);
        require_strict_upgrade(&Version::new(2, 46, 0), &installed).unwrap();
        for candidate in [Version::new(2, 45, 0), Version::new(2, 44, 0)] {
            assert!(matches!(
                require_strict_upgrade(&candidate, &installed),
                Err(UpdateError::Rollback { .. })
            ));
        }
    }
}
