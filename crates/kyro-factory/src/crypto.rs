//! Domain-separated RS256 seals with an operator-pinned public key map.
use crate::{Result, digest, fail};
use jsonwebtoken::{
    Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, decode_header, encode,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use zeroize::Zeroizing;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    Catalogue,
    Composition,
    Evidence,
    Release,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Seal {
    schema_version: u32,
    issuer: String,
    purpose: Purpose,
    digest: String,
}
pub struct Signer {
    id: String,
    key: EncodingKey,
    purpose: Purpose,
}
impl Signer {
    pub fn from_pem(id: String, purpose: Purpose, pem: Zeroizing<Vec<u8>>) -> Result<Self> {
        if !crate::label(&id) || pem.len() > 16384 {
            return Err(fail("invalid_signing_identity", "/signer"));
        }
        let key =
            EncodingKey::from_rsa_pem(&pem).map_err(|_| fail("invalid_signing_key", "/signer"))?;
        Ok(Self { id, key, purpose })
    }
    pub fn sign(&self, value: &impl Serialize) -> Result<String> {
        let seal = Seal {
            schema_version: 1,
            issuer: self.id.clone(),
            purpose: self.purpose.clone(),
            digest: digest(value)?,
        };
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.id.clone());
        encode(&header, &seal, &self.key).map_err(|_| fail("signature_unavailable", "/signature"))
    }
    pub fn purpose(&self) -> &Purpose {
        &self.purpose
    }
}
#[derive(Clone)]
pub struct PublicIdentity {
    pub id: String,
    pub purposes: BTreeSet<Purpose>,
    pub pem: Vec<u8>,
}
#[derive(Clone)]
pub struct Trust {
    keys: BTreeMap<String, (DecodingKey, BTreeSet<Purpose>)>,
}
impl Trust {
    pub fn new(identities: Vec<PublicIdentity>) -> Result<Self> {
        if identities.is_empty() || identities.len() > 16 {
            return Err(fail("invalid_trust_set", "/trust"));
        }
        let mut keys = BTreeMap::new();
        let mut fingerprints = BTreeSet::new();
        for identity in identities {
            if !crate::label(&identity.id)
                || identity.pem.len() > 16384
                || identity.purposes.len() != 1
                || keys.contains_key(&identity.id)
            {
                return Err(fail("invalid_trust_identity", "/trust"));
            }
            let normalized: Vec<u8> = identity
                .pem
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            if !normalized.starts_with(b"-----BEGINPUBLICKEY-----")
                || !fingerprints.insert(crate::digest_bytes(&normalized))
            {
                return Err(fail("signing_roles_must_use_distinct_keys", "/trust"));
            }
            let key = DecodingKey::from_rsa_pem(&identity.pem)
                .map_err(|_| fail("invalid_public_key", "/trust"))?;
            keys.insert(identity.id, (key, identity.purposes));
        }
        Ok(Self { keys })
    }
    pub fn verify(&self, purpose: Purpose, value: &impl Serialize, signature: &str) -> Result<()> {
        if signature.len() > 8192 {
            return Err(fail("signature_invalid", "/signature"));
        }
        let header =
            decode_header(signature).map_err(|_| fail("signature_invalid", "/signature"))?;
        if header.alg != Algorithm::RS256
            || header.jku.is_some()
            || header.jwk.is_some()
            || header.x5u.is_some()
        {
            return Err(fail("signature_invalid", "/signature"));
        }
        let id = header
            .kid
            .ok_or(fail("signature_identity_missing", "/signature"))?;
        let (key, allowed) = self
            .keys
            .get(&id)
            .filter(|(_, uses)| uses.contains(&purpose))
            .ok_or(fail("signature_identity_refused", "/signature"))?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.required_spec_claims.clear();
        validation.validate_exp = false;
        validation.validate_aud = false;
        let seal = decode::<Seal>(signature, key, &validation)
            .map_err(|_| fail("signature_invalid", "/signature"))?
            .claims;
        if seal.schema_version != 1
            || seal.issuer != id
            || seal.purpose != purpose
            || !allowed.contains(&seal.purpose)
            || seal.digest != digest(value)?
        {
            return Err(fail("signature_binding_mismatch", "/signature"));
        }
        Ok(())
    }
}
