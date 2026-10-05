use crate::{
    Result,
    crypto::{Purpose, Signer, Trust},
    digest, fail,
};
use kyro_app::contract::Schema;
use kyro_domain::factory::valid_digest;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Port {
    pub component_id: String,
    pub data_type: String,
    pub required: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentManifest {
    pub schema_version: u32,
    pub id: String,
    pub version: String,
    pub source_digest: String,
    pub source_files: BTreeMap<String, String>,
    pub migration_digests: BTreeMap<String, String>,
    pub configuration: Schema,
    pub dependencies: BTreeMap<String, BTreeSet<String>>,
    pub capabilities: BTreeSet<String>,
    pub ports: BTreeMap<String, Port>,
    pub output_type: Option<String>,
    pub effects: BTreeSet<String>,
    pub qualification_digest: String,
    pub criteria_digest: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedManifest {
    pub manifest: ComponentManifest,
    pub signature: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Admission {
    Pending,
    Admitted,
    Deprecated,
    Revoked,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub component: SignedManifest,
    pub admission: Admission,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub qualification: Option<SignedComponentQualification>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseReceipt {
    pub input_digest: String,
    pub observed_digest: String,
    pub passed: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentQualification {
    pub kind: String,
    pub schema_version: u32,
    pub component_id: String,
    pub version: String,
    pub subject_digest: String,
    pub source_digest: String,
    pub criteria_digest: String,
    pub verifier_version: String,
    pub run_id: uuid::Uuid,
    pub cases: BTreeMap<String, CaseReceipt>,
    pub report_digest: String,
    pub validation_environment: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedComponentQualification {
    pub qualification: ComponentQualification,
    pub signature: String,
}
impl SignedComponentQualification {
    pub fn verify(&self, manifest: &ComponentManifest, trust: &Trust) -> Result<()> {
        trust.verify(Purpose::Evidence, &self.qualification, &self.signature)?;
        let q = &self.qualification;
        let required = BTreeSet::from(["nominal", "refusal", "failure", "invariant"]);
        if q.kind != "component_qualification"
            || q.schema_version != 1
            || q.component_id != manifest.id
            || q.version != manifest.version
            || q.subject_digest != manifest.qualification_subject_digest()?
            || q.source_digest != manifest.source_digest
            || q.criteria_digest != manifest.criteria_digest
            || q.verifier_version != "kyro-component-verifier-1"
            || q.run_id.is_nil()
            || q.validation_environment != "synthetic_integration"
            || !valid_digest(&q.report_digest)
            || q.cases.keys().map(String::as_str).collect::<BTreeSet<_>>() != required
            || q.cases.values().any(|r| {
                !r.passed || !valid_digest(&r.input_digest) || !valid_digest(&r.observed_digest)
            })
            || digest(self)? != manifest.qualification_digest
        {
            return Err(fail(
                "component_qualification_invalid",
                format!("/components/{}", manifest.id),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Catalogue {
    pub schema_version: u32,
    pub revision: u64,
    pub entries: BTreeMap<String, BTreeMap<String, Entry>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedCatalogue {
    pub catalogue: Catalogue,
    pub signature: String,
}
pub fn component_id(id: &str) -> bool {
    kyro_domain::factory::valid_component_id(id)
}
pub fn version(v: &str) -> bool {
    v.len() <= 32
        && v.split('.').count() == 3
        && v.split('.').all(|s| {
            !s.is_empty()
                && s.len() <= 5
                && s.bytes().all(|b| b.is_ascii_digit())
                && (s.len() == 1 || !s.starts_with('0'))
        })
}
pub fn source_path(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 240
        && !s.starts_with('/')
        && !s.contains('\\')
        && s.split('/').all(|p| {
            !p.is_empty()
                && p != "."
                && p != ".."
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}
impl ComponentManifest {
    /// Exclude only the evidence reference to avoid a circular content hash.
    /// The criteria, contracts, effects, dependencies and every source remain bound.
    pub fn qualification_subject_digest(&self) -> Result<String> {
        let mut subject = self.clone();
        subject.qualification_digest = "0".repeat(64);
        digest(&subject)
    }
    pub fn validate(&self) -> Result<()> {
        let path = format!("/components/{}/{}", self.id, self.version);
        if self.schema_version != 1
            || !component_id(&self.id)
            || !version(&self.version)
            || !valid_digest(&self.source_digest)
            || !valid_digest(&self.qualification_digest)
            || !valid_digest(&self.criteria_digest)
            || self.source_files.is_empty()
            || self.source_files.len() > 1024
            || self.migration_digests.len() > 128
            || self.dependencies.len() > 32
            || self.capabilities.len() > 32
            || self.ports.len() > 16
            || self.effects.len() > 8
        {
            return Err(fail("manifest_invalid", path));
        }
        for (p, h) in self.source_files.iter().chain(&self.migration_digests) {
            if !source_path(p) || !valid_digest(h) {
                return Err(fail("manifest_source_invalid", path));
            }
        }
        if digest(&self.source_files)? != self.source_digest {
            return Err(fail("manifest_source_digest_mismatch", path));
        }
        for (id, versions) in &self.dependencies {
            if !component_id(id)
                || id == &self.id
                || versions.is_empty()
                || versions.len() > 16
                || versions.iter().any(|v| !version(v))
            {
                return Err(fail("manifest_dependency_invalid", path));
            }
        }
        if self.capabilities.iter().any(|c| !crate::label(c))
            || self.effects.iter().any(|e| {
                !matches!(
                    e.as_str(),
                    "read" | "write" | "network" | "credential" | "budget"
                )
            })
            || self.ports.iter().any(|(name, p)| {
                !crate::label(name) || !component_id(&p.component_id) || !crate::label(&p.data_type)
            })
            || self
                .output_type
                .as_deref()
                .is_some_and(|s| !crate::label(s))
        {
            return Err(fail("manifest_contract_invalid", path));
        }
        self.configuration
            .validate_definition()
            .map_err(|_| fail("manifest_configuration_invalid", path))
    }
}
impl SignedCatalogue {
    /// Publication preserves every old version and permanent revocation. A
    /// corrected source/contract is a new version, including before admission.
    pub fn verify_successor(&self, next: &Self, trust: &Trust) -> Result<()> {
        self.verify(trust, self.catalogue.revision)?;
        let minimum = self
            .catalogue
            .revision
            .checked_add(1)
            .ok_or(fail("catalogue_revision_exhausted", "/revision"))?;
        next.verify(trust, minimum)?;
        for (id, versions) in &self.catalogue.entries {
            for (version, old) in versions {
                let new = next
                    .catalogue
                    .entries
                    .get(id)
                    .and_then(|v| v.get(version))
                    .ok_or(fail("catalogue_history_missing", id))?;
                if old.component.manifest.qualification_subject_digest()?
                    != new.component.manifest.qualification_subject_digest()?
                    || (old.admission == Admission::Revoked && new.admission != Admission::Revoked)
                    || old.qualification.as_ref().is_some_and(|q| {
                        new.qualification
                            .as_ref()
                            .is_none_or(|n| digest(q).ok() != digest(n).ok())
                    })
                {
                    return Err(fail("catalogue_version_is_immutable", id));
                }
            }
        }
        Ok(())
    }

    /// Evidence is sealed by the independent qualification role. The registry
    /// role may admit that exact subject, but cannot manufacture its receipt.
    pub fn admit(
        &self,
        proof: SignedComponentQualification,
        trust: &Trust,
        signer: &Signer,
    ) -> Result<Self> {
        self.admit_batch(vec![proof], trust, signer)
    }

    /// One immutable publication for a bounded qualification campaign. A bad
    /// receipt refuses the entire batch; no intermediate revision is exposed.
    pub fn admit_batch(
        &self,
        proofs: Vec<SignedComponentQualification>,
        trust: &Trust,
        signer: &Signer,
    ) -> Result<Self> {
        self.verify(trust, self.catalogue.revision)?;
        if signer.purpose() != &Purpose::Catalogue {
            return Err(fail("catalogue_signer_required", "/signer"));
        }
        if proofs.is_empty() || proofs.len() > 147 {
            return Err(fail("qualification_batch_invalid", "/qualification"));
        }
        let mut catalogue = self.catalogue.clone();
        let mut subjects = BTreeSet::new();
        let mut changed = false;
        for proof in proofs {
            let q = &proof.qualification;
            if !subjects.insert((q.component_id.clone(), q.version.clone())) {
                return Err(fail("qualification_batch_duplicate", "/qualification"));
            }
            let entry = catalogue
                .entries
                .get_mut(&q.component_id)
                .and_then(|versions| versions.get_mut(&q.version))
                .ok_or(fail("component_absent", "/qualification"))?;
            if entry.admission == Admission::Revoked {
                return Err(fail("revocation_is_permanent", "/qualification"));
            }
            if entry
                .qualification
                .as_ref()
                .is_some_and(|old| digest(old).ok() != digest(&proof).ok())
            {
                return Err(fail("qualification_version_is_immutable", "/qualification"));
            }
            entry.component.manifest.qualification_digest = digest(&proof)?;
            proof.verify(&entry.component.manifest, trust)?;
            if entry.admission == Admission::Admitted {
                continue;
            }
            entry.component.signature = signer.sign(&entry.component.manifest)?;
            entry.qualification = Some(proof);
            entry.admission = Admission::Admitted;
            changed = true;
        }
        if !changed {
            return Ok(self.clone());
        }
        catalogue.revision = catalogue
            .revision
            .checked_add(1)
            .ok_or(fail("catalogue_revision_exhausted", "/revision"))?;
        let result = Self {
            signature: signer.sign(&catalogue)?,
            catalogue,
        };
        result.verify(trust, self.catalogue.revision + 1)?;
        Ok(result)
    }

    pub fn verify(&self, trust: &Trust, minimum_revision: u64) -> Result<()> {
        if self.catalogue.schema_version != 1
            || self.catalogue.revision < minimum_revision
            || self.catalogue.revision == 0
            || self.catalogue.entries.is_empty()
            || self.catalogue.entries.len() > 147
            || serde_json::to_vec(self)
                .map_err(|_| fail("catalogue_invalid", "/"))?
                .len()
                > 8388608
        {
            return Err(fail("catalogue_revision_refused", "/catalogue"));
        }
        trust.verify(Purpose::Catalogue, &self.catalogue, &self.signature)?;
        for (id, versions) in &self.catalogue.entries {
            if versions.is_empty() || versions.len() > 16 {
                return Err(fail("catalogue_versions_invalid", id));
            }
            for (v, entry) in versions {
                let m = &entry.component.manifest;
                if id != &m.id || v != &m.version {
                    return Err(fail("catalogue_key_mismatch", id));
                }
                m.validate()?;
                trust.verify(Purpose::Catalogue, m, &entry.component.signature)?;
                match &entry.qualification {
                    Some(proof) => proof.verify(m, trust)?,
                    None if matches!(
                        entry.admission,
                        Admission::Admitted | Admission::Deprecated
                    ) =>
                    {
                        return Err(fail("component_qualification_missing", id));
                    }
                    None => {}
                }
            }
        }
        Ok(())
    }
    pub fn admitted(&self, id: &str, version: &str) -> Result<&ComponentManifest> {
        let entry = self
            .catalogue
            .entries
            .get(id)
            .and_then(|v| v.get(version))
            .ok_or(fail(
                "component_absent",
                format!("/components/{id}/{version}"),
            ))?;
        if entry.admission != Admission::Admitted {
            return Err(fail(
                match entry.admission {
                    Admission::Revoked => "component_revoked",
                    Admission::Deprecated => "component_deprecated",
                    _ => "component_not_admitted",
                },
                format!("/components/{id}/{version}"),
            ));
        }
        Ok(&entry.component.manifest)
    }
    pub fn changed(
        &self,
        id: &str,
        version: &str,
        admission: Admission,
        signer: &Signer,
    ) -> Result<Self> {
        if signer.purpose() != &Purpose::Catalogue {
            return Err(fail("catalogue_signer_required", "/signer"));
        }
        let mut catalogue = self.catalogue.clone();
        catalogue.revision = catalogue
            .revision
            .checked_add(1)
            .ok_or(fail("catalogue_revision_exhausted", "/revision"))?;
        let entry = catalogue
            .entries
            .get_mut(id)
            .and_then(|v| v.get_mut(version))
            .ok_or(fail("component_absent", id))?;
        // A revoked digest is permanent; a corrected implementation needs a version.
        if entry.admission == Admission::Revoked {
            return Err(fail("revocation_is_permanent", id));
        }
        if admission == Admission::Admitted && entry.qualification.is_none() {
            return Err(fail("component_qualification_missing", id));
        }
        entry.admission = admission;
        let signature = signer.sign(&catalogue)?;
        Ok(Self {
            catalogue,
            signature,
        })
    }
}
