//! Metadata of a release candidate, written by `build-candidate.sh`: the
//! pinned installer, the SPDX SBOM and `candidate.json`. Each file is
//! canonical JSON (or shell) derived only from its arguments, so equal
//! inputs give equal bytes.
use super::files::{canonical_json, write};
use super::syntax::shell_quote;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

/// `render-installer TEMPLATE OUTPUT VERSION COMMIT HOST_URL HOST_SHA256
/// GUEST_URL GUEST_SHA256 HOST_PATH GUEST_PATH`: replaces each
/// `__HAMN_*__` placeholder, which must occur exactly once, with its
/// shell-quoted value; no placeholder may remain.
pub fn render_installer(args: &[String]) -> Result<(), String> {
    let [template, output, version, commit, host_url, host_sha256, guest_url, guest_sha256, host_path, guest_path] =
        args
    else {
        return Err("usage: hamn-dev release render-installer TEMPLATE OUTPUT VERSION COMMIT HOST_URL HOST_SHA256 \
                    GUEST_URL GUEST_SHA256 HOST_PATH GUEST_PATH"
            .into());
    };
    let size =
        |path: &str| fs::metadata(path).map(|info| info.len().to_string()).map_err(|error| format!("{path}: {error}"));
    let values = [
        ("__HAMN_VERSION__", version.clone()),
        ("__HAMN_COMMIT__", commit.clone()),
        ("__HAMN_HOST_URL__", host_url.clone()),
        ("__HAMN_HOST_SHA256__", host_sha256.clone()),
        ("__HAMN_GUEST_URL__", guest_url.clone()),
        ("__HAMN_GUEST_SHA256__", guest_sha256.clone()),
        ("__HAMN_HOST_SIZE__", size(host_path)?),
        ("__HAMN_GUEST_SIZE__", size(guest_path)?),
    ];
    let template = fs::read_to_string(template).map_err(|error| format!("{template}: {error}"))?;
    let rendered = render(&template, &values)?;
    write(Path::new(output), rendered.as_bytes())
}

fn render(template: &str, values: &[(&str, String)]) -> Result<String, String> {
    let mut rendered = template.to_owned();
    for (placeholder, value) in values {
        if rendered.matches(placeholder).count() != 1 {
            return Err(format!("installer template placeholder is malformed: {placeholder}"));
        }
        rendered = rendered.replace(placeholder, &shell_quote(value));
    }
    if rendered.contains("__HAMN_") {
        return Err("installer template has an unresolved placeholder".into());
    }
    Ok(rendered)
}

/// `YYYY-MM-DDTHH:MM:SSZ` for seconds since the Unix epoch (years up to 9999).
pub fn utc_timestamp(epoch: u64) -> Result<String, String> {
    if epoch > 253_402_300_799 {
        return Err(format!("timestamp {epoch} is after 9999-12-31"));
    }
    let (days, seconds) = ((epoch / 86_400) as i64, epoch % 86_400);
    // Howard Hinnant's days-to-civil conversion for the proleptic Gregorian calendar.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 { month_index + 3 } else { month_index - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    Ok(format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", seconds / 3600, seconds / 60 % 60, seconds % 60))
}

/// `write-sbom OUTPUT VERSION COMMIT TREE COMMIT_EPOCH HOST_NAME HOST_SHA256
/// GUEST_NAME GUEST_SHA256`
pub fn write_sbom(args: &[String]) -> Result<(), String> {
    let [output, version, commit, tree, epoch, host_name, host_sha256, guest_name, guest_sha256] = args else {
        return Err(
            "usage: hamn-dev release write-sbom OUTPUT VERSION COMMIT TREE COMMIT_EPOCH HOST_NAME HOST_SHA256 \
                    GUEST_NAME GUEST_SHA256"
                .into(),
        );
    };
    let epoch: u64 = epoch.parse().map_err(|_| format!("commit timestamp is invalid: {epoch}"))?;
    let package = |id: &str, name: &str, digest: &str| {
        json!({
            "SPDXID": id,
            "name": name,
            "versionInfo": version,
            "downloadLocation": "NOASSERTION",
            "filesAnalyzed": false,
            "checksums": [{"algorithm": "SHA256", "checksumValue": digest}],
        })
    };
    let document = json!({
        "SPDXID": "SPDXRef-DOCUMENT",
        "spdxVersion": "SPDX-2.3",
        "name": format!("Hamn {version}"),
        "dataLicense": "CC0-1.0",
        "documentNamespace": format!("https://hamn.dev/spdx/{version}/{commit}"),
        "creationInfo": {
            "creators": ["Tool: hamn-release-candidate"],
            "created": utc_timestamp(epoch)?,
            "licenseListVersion": "3.23",
        },
        "packages": [
            package("SPDXRef-HamnHost", host_name, host_sha256),
            package("SPDXRef-HamnGuest", guest_name, guest_sha256),
        ],
        "annotations": [{
            "annotationType": "OTHER",
            "annotator": "Tool: hamn-release-candidate",
            "comment": format!("commit={commit} sourceTree={tree}"),
        }],
    });
    write(Path::new(output), canonical_json(&document).as_bytes())
}

/// `write-candidate OUTPUT TAG VERSION COMMIT TREE` followed by four
/// `NAME SHA256` pairs (host, guest, installer, SBOM).
pub fn write_candidate(args: &[String]) -> Result<(), String> {
    let usage = "usage: hamn-dev release write-candidate OUTPUT TAG VERSION COMMIT TREE \
                 HOST_NAME HOST_SHA256 GUEST_NAME GUEST_SHA256 INSTALLER_NAME INSTALLER_SHA256 SBOM_NAME SBOM_SHA256";
    let [output, tag, version, commit, tree, artifacts @ ..] = args else {
        return Err(usage.into());
    };
    if artifacts.len() != 8 {
        return Err(usage.into());
    }
    let artifacts: Vec<Value> = artifacts.chunks(2).map(|pair| json!({"name": pair[0], "sha256": pair[1]})).collect();
    let document = json!({
        "schemaVersion": 1,
        "kind": "hamn-release-candidate",
        "tag": tag,
        "version": version,
        "commit": commit,
        "sourceTree": tree,
        "artifacts": artifacts,
    });
    write(Path::new(output), canonical_json(&document).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_utc_iso_8601() {
        assert_eq!(utc_timestamp(0).unwrap(), "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamp(951_782_400).unwrap(), "2000-02-29T00:00:00Z");
        assert_eq!(utc_timestamp(1_700_000_000).unwrap(), "2023-11-14T22:13:20Z");
        assert_eq!(utc_timestamp(1_709_251_199).unwrap(), "2024-02-29T23:59:59Z");
        assert_eq!(utc_timestamp(253_402_300_799).unwrap(), "9999-12-31T23:59:59Z");
        assert!(utc_timestamp(253_402_300_800).is_err());
    }

    #[test]
    fn installer_placeholders_are_quoted_once_and_all_resolved() {
        let values = [("__HAMN_VERSION__", "v0.0.1".to_owned()), ("__HAMN_HOST_URL__", "file:///tmp/a b".to_owned())];
        assert_eq!(
            render("V=__HAMN_VERSION__\nU=__HAMN_HOST_URL__\n", &values).unwrap(),
            "V=v0.0.1\nU='file:///tmp/a b'\n"
        );
        let missing = render("V=__HAMN_VERSION__\n", &values).unwrap_err();
        assert!(missing.contains("malformed: __HAMN_HOST_URL__"), "{missing}");
        let twice = render("__HAMN_VERSION__ __HAMN_VERSION__ __HAMN_HOST_URL__", &values).unwrap_err();
        assert!(twice.contains("malformed: __HAMN_VERSION__"), "{twice}");
        let extra = render("__HAMN_VERSION__ __HAMN_HOST_URL__ __HAMN_OTHER__", &values).unwrap_err();
        assert!(extra.contains("unresolved placeholder"), "{extra}");
    }
}
