//! KV map-name construction.
//!
//! Every map this contract touches lives in the tenant's z-namespace and is
//! named `z:<tenant-did-hex>:<tail>` (`docs/INTERFACE.md` §"Maps").
//!
//! **The tenant DID is raw bytes.** `tenant_context::tenant_did()` returns
//! `list<u8>`; the hex encoding happens exactly once, here. Hex-encoding an
//! already-encoded DID (or not encoding it at all) silently matches no map, so
//! all map names must come from [`Namespace`] rather than `format!` at call sites.

/// `z:<tid>:policy` — the active policy document (key `current`).
pub const POLICY_TAIL: &str = "policy";
/// `z:<tid>:audit` — append-only audit ledger (`seq:<000001>` records + `exp:<id>` pointers).
pub const AUDIT_TAIL: &str = "audit";
/// `z:<tid>:fx` — cached exchange rates (key `<FROM>:<TO>`).
pub const FX_TAIL: &str = "fx";
/// `z:<tid>:secrets` — tenant-provisioned secrets (key `approval_webhook_url`).
pub const SECRETS_TAIL: &str = "secrets";

/// A tenant-scoped view of the four maps this contract uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Namespace {
    tenant_hex: String,
}

impl Namespace {
    /// Build the namespace for a raw tenant DID. The DID is hex-encoded
    /// exactly once, right here.
    pub fn new(tenant_did: &[u8]) -> Self {
        Self {
            tenant_hex: hex::encode(tenant_did),
        }
    }

    /// The hex form of the tenant DID, as used inside every map name.
    pub fn tenant_hex(&self) -> &str {
        &self.tenant_hex
    }

    /// `z:<tid>:<tail>`.
    pub fn canonical(&self, tail: &str) -> String {
        format!("z:{}:{}", self.tenant_hex, tail)
    }

    /// `z:<tid>:policy`.
    pub fn policy(&self) -> String {
        self.canonical(POLICY_TAIL)
    }

    /// `z:<tid>:audit`.
    pub fn audit(&self) -> String {
        self.canonical(AUDIT_TAIL)
    }

    /// `z:<tid>:fx`.
    pub fn fx(&self) -> String {
        self.canonical(FX_TAIL)
    }

    /// `z:<tid>:secrets`.
    pub fn secrets(&self) -> String {
        self.canonical(SECRETS_TAIL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A realistic 32-byte DID with bytes that would be easy to mangle.
    const RAW_DID: [u8; 8] = [0x8f, 0x3a, 0x00, 0xff, 0x10, 0xab, 0x7e, 0x42];
    const HEX_DID: &str = "8f3a00ff10ab7e42";

    #[test]
    fn raw_tenant_did_is_hex_encoded_exactly_once() {
        let namespace = Namespace::new(&RAW_DID);
        assert_eq!(namespace.tenant_hex(), HEX_DID);
        assert_eq!(namespace.policy(), "z:8f3a00ff10ab7e42:policy");
        assert_eq!(namespace.audit(), "z:8f3a00ff10ab7e42:audit");
        assert_eq!(namespace.fx(), "z:8f3a00ff10ab7e42:fx");
        assert_eq!(namespace.secrets(), "z:8f3a00ff10ab7e42:secrets");
    }

    #[test]
    fn ascii_tenant_did_is_not_double_encoded() {
        // The DID here *is* the ASCII text "8f3a"; a double hex-encode would
        // yield "38663361" and match nothing on the host.
        let namespace = Namespace::new(b"8f3a");
        assert_eq!(namespace.tenant_hex(), "38663361");
        assert_ne!(namespace.tenant_hex(), "8f3a");
        assert_eq!(namespace.audit(), "z:38663361:audit");
    }

    #[test]
    fn canonical_accepts_any_tail_and_keeps_the_z_prefix() {
        let namespace = Namespace::new(&RAW_DID);
        assert_eq!(
            namespace.canonical("custom-tail"),
            "z:8f3a00ff10ab7e42:custom-tail"
        );
        assert!(namespace.canonical(POLICY_TAIL).starts_with("z:"));
    }

    #[test]
    fn map_names_are_distinct_per_tenant() {
        let other = Namespace::new(&[0x01, 0x02]);
        assert_ne!(Namespace::new(&RAW_DID).audit(), other.audit());
    }
}
