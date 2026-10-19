//! Signature of the catalog index: minisign (Ed25519). The keys of the
//! official catalog are built into the app; the owner of a pedalboard can
//! trust more (their own catalog) by listing them, one per line, in
//! `<data dir>/store/trusted-keys.pub`. Plugins copied by hand into the
//! LV2 folder are never checked: the signature only guards what the store
//! downloads.

use anyhow::{anyhow, bail, Result};
use minisign_verify::{PublicKey, Signature};
use std::path::Path;

/// Public keys of the official catalog (the base64 line of `minisign.pub`).
/// Several, so that a key can be replaced without breaking older apps.
pub const CATALOG_KEYS: &[&str] = &[
    // minisign key 92A010E3E7AE8C1C (2026-10-19)
    "RWQcjK7n4xCgkjDBftDzm+RuQjOyHNEVdEosorqIazwWQs3YDARyRvMk",
];

/// File listing the extra keys the owner trusts
pub const TRUSTED_KEYS_FILE: &str = "trusted-keys.pub";

/// The keys an index may be signed with: the official ones, then the
/// owner's (comment lines, `untrusted comment:` headers and blank lines
/// are skipped, so a `minisign.pub` file can be pasted as is)
pub fn trusted_keys(store_dir: &Path) -> Vec<String> {
    let mut keys: Vec<String> = CATALOG_KEYS.iter().map(|k| k.to_string()).collect();
    if let Ok(text) = std::fs::read_to_string(store_dir.join(TRUSTED_KEYS_FILE)) {
        keys.extend(
            text.lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.contains(':'))
                .map(str::to_string),
        );
    }
    keys
}

/// Check that `signature` (the text of a `.minisig` file) signs `data`
/// with one of `keys`
pub fn verify(data: &[u8], signature: &str, keys: &[String]) -> Result<()> {
    if keys.is_empty() {
        bail!("This build of OpenRig has no plugin catalog key");
    }
    let signature =
        Signature::decode(signature).map_err(|_| anyhow!("Unreadable catalog signature"))?;
    for key in keys {
        let Ok(key) = PublicKey::from_base64(key) else {
            tracing::warn!("Ignoring an invalid catalog key");
            continue;
        };
        if key.verify(data, &signature, false).is_ok() {
            return Ok(());
        }
    }
    bail!("The plugin catalog is not signed by a trusted key")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Test vector of the minisign-verify crate (not a catalog key)
    const KEY: &str = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
    const SIGNATURE: &str = "untrusted comment: signature from minisign secret key
RUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=
trusted comment: timestamp:1556193335\tfile:test
y/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";

    #[test]
    fn checks_the_signature() {
        let keys = vec![KEY.to_string()];
        assert!(verify(b"test", SIGNATURE, &keys).is_ok());
        assert!(verify(b"tampered", SIGNATURE, &keys).is_err());
        assert!(verify(b"test", SIGNATURE, &[]).is_err());
    }

    #[test]
    fn reads_the_owner_keys() {
        let dir = std::env::temp_dir().join(format!("openrig-keys-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(TRUSTED_KEYS_FILE),
            format!("untrusted comment: minisign public key\n{KEY}\n\n# mine\n"),
        )
        .unwrap();
        assert!(trusted_keys(&dir).contains(&KEY.to_string()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
