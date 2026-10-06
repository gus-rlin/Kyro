//! The complete closed P2 registry, initially pending. Declaring an action or
//! hashing its implementation never admits a component without observed cases.
use crate::{
    Result,
    catalogue::*,
    crypto::{Purpose, Signer},
    digest, fail,
};
use kyro_app::contract::Schema;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};
include!("component_names.rs");

pub const VERSION: &str = "0.2.0";
pub fn actions(id: &str) -> Result<BTreeSet<String>> {
    let factory = match id {
        "B161" => Some(&["validate_spec"][..]),
        "B162" => Some(
            &[
                "publish_catalogue",
                "admit_component",
                "change_admission",
                "read_catalogue",
            ][..],
        ),
        "B163" => Some(&["resolve_lock", "verify_lock"][..]),
        "B164" => Some(&["assemble_sources", "compile_offline", "package_oci"][..]),
        "B169" => Some(&["build_application", "cancel_build"][..]),
        "B171" => Some(
            &[
                "launch_builder",
                "launch_verifier",
                "cancel_sandbox",
                "reap_expired",
            ][..],
        ),
        "B172" => Some(&["verify_candidate", "attest_evidence"][..]),
        "B173" => Some(
            &[
                "read_artifact",
                "export_sources",
                "export_git",
                "export_oci",
                "attest_release",
            ][..],
        ),
        _ => None,
    };
    if let Some(actions) = factory {
        return Ok(actions.iter().map(|s| s.to_string()).collect());
    }
    kyro_app::operations::component_actions(id)
        .map_err(|_| fail("component_contract_missing", format!("/components/{id}")))
}
/// Defaults are payload fields, not arbitrary code or provider configuration.
/// A node restricts allowed_actions when a default is specific to one operation.
fn configuration(id: &str, actions: &BTreeSet<String>) -> Schema {
    let string = || Schema::String {
        max_length: 128,
        values: BTreeSet::new(),
    };
    Schema::Object {
        properties: BTreeMap::from([
            ("reference".into(), string()),
            (
                "allowed_actions".into(),
                Schema::Array {
                    items: Box::new(Schema::String {
                        max_length: 128,
                        values: actions.clone(),
                    }),
                    max_items: 64,
                },
            ),
            (
                "defaults".into(),
                Schema::Object {
                    properties: kyro_app::composition::default_fields(id)
                        .iter()
                        .map(|s| ((*s).into(), string()))
                        .collect(),
                    required: BTreeSet::new(),
                    additional: false,
                },
            ),
        ]),
        required: BTreeSet::new(),
        additional: false,
    }
}
fn links(
    id: &str,
) -> (
    &'static [&'static str],
    &'static [(&'static str, &'static str)],
) {
    match id {
        "B032" | "B034" | "B035" | "B036" | "B037" | "B038" | "B039" => {
            (&["B031"], &[("entity", "B031")])
        }
        "B040" => (&["B031", "B052"], &[("entity", "B031")]),
        "B053" => (&["B052"], &[]),
        "B058" | "B059" => (&["B054", "B157"], &[]),
        "B083" | "B084" | "B086" | "B089" => (&["B081"], &[]),
        "B094" => (&["B052", "B091", "B093"], &[]),
        "B092" | "B095" | "B096" | "B097" | "B099" | "B100" => (&["B052"], &[]),
        "B112" => (&["B111"], &[("resource_id", "B111")]),
        "B113" => (&["B111", "B112"], &[("resource_id", "B111")]),
        "B114" => (&["B113"], &[("slot_id", "B113")]),
        "B115" => (&["B114"], &[("booking_id", "B114")]),
        "B116" => (&["B113"], &[("slot_id", "B113")]),
        "B117" => (&["B111", "B112"], &[("resource_id", "B111")]),
        "B118" => (&["B111"], &[("establishment_id", "B111")]),
        "B119" => (&["B114"], &[("booking_id", "B114")]),
        "B120" => (&["B054", "B114", "B154"], &[]),
        "B122" => (&["B121"], &[("product_id", "B121")]),
        "B123" => (&["B121", "B122"], &[]),
        "B124" => (&["B123"], &[("quote_id", "B123")]),
        "B125" => (&["B054", "B124", "B153"], &[("order_id", "B124")]),
        "B126" => (&["B054", "B122", "B153"], &[("price_id", "B122")]),
        "B127" => (&["B124"], &[("order_id", "B124")]),
        "B128" => (&["B054", "B125", "B153"], &[("payment_id", "B125")]),
        "B130" => (&["B121"], &[("product_id", "B121")]),
        "B136" => (&["B134"], &[("project_id", "B134")]),
        "B144" => (&["B143"], &[]),
        "B145" => (&["B144"], &[]),
        "B147" | "B149" => (&["B143"], &[]),
        "B104" => (&["B054", "B152"], &[]),
        "B105" | "B106" | "B151" | "B152" | "B153" | "B154" | "B155" | "B156" | "B157" | "B158"
        | "B160" => (&["B054"], &[]),
        "B163" => (&["B161", "B162"], &[]),
        "B164" => (&["B163", "B171"], &[]),
        "B169" => (&["B164", "B172", "B173"], &[]),
        "B172" => (&["B171"], &[]),
        "B173" => (&["B172"], &[]),
        _ => (&[], &[]),
    }
}
fn effects(id: &str, actions: &BTreeSet<String>) -> Result<BTreeSet<String>> {
    let mut result = BTreeSet::new();
    for action in actions {
        let read = if id[1..].parse::<u16>().is_ok_and(|n| n <= 160) {
            kyro_app::operations::component_is_read(id, action)
                .map_err(|_| fail("component_contract_missing", id))?
        } else {
            matches!(
                action.as_str(),
                "validate_spec"
                    | "read_catalogue"
                    | "verify_lock"
                    | "read_artifact"
                    | "export_sources"
                    | "export_git"
                    | "export_oci"
            )
        };
        result.insert(if read { "read" } else { "write" }.into());
    }
    if matches!(
        id,
        "B001"
            | "B006"
            | "B058"
            | "B059"
            | "B092"
            | "B094"
            | "B095"
            | "B096"
            | "B097"
            | "B099"
            | "B100"
            | "B104"
            | "B105"
            | "B106"
            | "B120"
            | "B125"
            | "B126"
            | "B128"
            | "B151"
            | "B152"
            | "B153"
            | "B154"
            | "B155"
            | "B156"
            | "B157"
            | "B158"
            | "B160"
    ) {
        result.extend(
            ["network", "credential", "budget"]
                .into_iter()
                .map(str::to_owned),
        );
    } else if matches!(id, "B002" | "B003" | "B004" | "B010") {
        result.insert("credential".into());
    }
    Ok(result)
}
/// Every entry shares the admitted runtime kernel, while selected blocks are
/// fixed in the compiled plan. Source provenance still names each owning block.
pub fn pending(root: &Path, revision: u64, signer: &Signer) -> Result<SignedCatalogue> {
    if revision == 0 || signer.purpose() != &Purpose::Catalogue {
        return Err(fail("catalogue_context_invalid", "/catalogue"));
    }
    let sources = crate::assembler::collect_runtime_sources(root)?;
    let migrations = sources
        .iter()
        .filter(|(p, _)| p.starts_with("crates/kyro-app/migrations/"))
        .map(|(p, h)| (p.clone(), h.clone()))
        .collect::<BTreeMap<_, _>>();
    let source_digest = digest(&sources)?;
    let mut entries = BTreeMap::new();
    for (id, name) in COMPONENT_NAMES {
        let actions = actions(id)?;
        let configuration = configuration(id, &actions);
        configuration.validate_definition().map_err(|_| {
            fail(
                "component_configuration_invalid",
                format!("/components/{id}"),
            )
        })?;
        let criteria = digest(
            &serde_json::json!({"schema_version":1,"component_id":id,"name":name,"version":VERSION,
            "actions":actions,"cases":["nominal","refusal","failure","invariant"],"environment":"synthetic_integration"}),
        )?;
        let (dependencies, ports) = links(id);
        let effects = effects(id, &actions)?;
        let component = ComponentManifest {
            schema_version: 1,
            id: id.to_string(),
            version: VERSION.into(),
            source_digest: source_digest.clone(),
            source_files: sources.clone(),
            migration_digests: migrations.clone(),
            configuration,
            dependencies: dependencies
                .iter()
                .map(|d| ((*d).into(), BTreeSet::from([VERSION.into()])))
                .collect(),
            capabilities: BTreeSet::from([format!("component.{id}").to_ascii_lowercase()]),
            ports: ports
                .iter()
                .map(|(name, component)| {
                    (
                        (*name).into(),
                        Port {
                            component_id: (*component).into(),
                            data_type: "reference".into(),
                            required: false,
                        },
                    )
                })
                .collect(),
            output_type: Some("reference".into()),
            effects,
            qualification_digest: "0".repeat(64),
            criteria_digest: criteria,
        };
        let signed = SignedManifest {
            signature: signer.sign(&component)?,
            manifest: component,
        };
        entries.insert(
            id.to_string(),
            BTreeMap::from([(
                VERSION.into(),
                Entry {
                    component: signed,
                    admission: Admission::Pending,
                    qualification: None,
                },
            )]),
        );
    }
    let catalogue = Catalogue {
        schema_version: 1,
        revision,
        entries,
    };
    Ok(SignedCatalogue {
        signature: signer.sign(&catalogue)?,
        catalogue,
    })
}
pub fn capabilities() -> BTreeSet<String> {
    COMPONENT_NAMES
        .iter()
        .map(|(id, _)| format!("component.{id}").to_ascii_lowercase())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_147_p2_ids_have_actual_closed_action_contracts() {
        assert_eq!(COMPONENT_NAMES.len(), 147);
        let mut ids = BTreeSet::new();
        for (id, _) in COMPONENT_NAMES {
            assert!(component_id(id));
            assert!(ids.insert(*id));
            let declared = actions(id).unwrap();
            assert!(!declared.is_empty(), "{id}");
            configuration(id, &declared).validate_definition().unwrap();
        }
        for excluded in ["B061", "B080", "B159", "B165", "B170", "B174", "B180"] {
            assert!(!ids.contains(excluded));
        }
    }
}
