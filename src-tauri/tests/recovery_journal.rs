#[path = "../src/recovery_journal.rs"]
mod recovery_journal;
use recovery_journal::*;
use sha2::{Digest, Sha256};

fn identity() -> SourceIdentity { SourceIdentity { bytes: 4096, sha256: [0x5a; 32] } }
fn rect(left: f32) -> RecoveryRect { RecoveryRect { left, bottom: 20.0, right: left + 20.0, top: 50.0 } }
fn text_highlight_rect() -> RecoveryRect { RecoveryRect { left: 40.0, bottom: 20.0, right: 90.0, top: 50.0 } }
fn record() -> RecoveryRecord {
    RecoveryRecord {
        active: true,
        generation: 7, revision: 4, current_page: 1, source_pages: 3, source: identity(),
        edit_state: RecoveryEditState { pages: vec![
            RecoveryPage { source: 2, turns: 1, crop: Some(RecoveryRect { left: 1.0, bottom: 2.0, right: 611.0, top: 790.0 }), annotations: vec![RecoveryAnnotation { id: "pdf-workstation-note-1".into(), kind: RecoveryAnnotationKind::Note, rect: rect(10.0), contents: "Private note text".into(), quads: vec![], created: None, modified: None }] },
            RecoveryPage { source: 0, turns: 0, crop: None, annotations: vec![RecoveryAnnotation { id: "pdf-workstation-highlight-2".into(), kind: RecoveryAnnotationKind::TextHighlight, rect: text_highlight_rect(), contents: String::new(), quads: vec![rect(40.0), rect(70.0)], created: None, modified: None }] },
        ] },
    }
}

fn resign(bytes: &mut Vec<u8>) { bytes.truncate(bytes.len() - 32); bytes.extend_from_slice(&Sha256::digest(&*bytes)); }
fn frame_with_state(state: &RecoveryEditState) -> Vec<u8> {
    let mut bytes = encode_recovery(&record()).unwrap();
    let payload = serde_json::to_vec(state).unwrap();
    bytes.truncate(74); bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes()); bytes.extend_from_slice(&payload);
    let digest = Sha256::digest(&bytes); bytes.extend_from_slice(&digest); bytes
}

#[test]
fn round_trips_only_source_identity_and_current_supported_edit_state() {
    let expected = record();
    let bytes = encode_recovery(&expected).unwrap();
    let decoded = decode_recovery(&bytes, identity(), Some(6), 4).unwrap();
    assert_eq!(decoded, expected);
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("Private note text"));
    for forbidden in ["C:\\", "/Users/", "password", "%PDF", "document body"] { assert!(!text.contains(forbidden)); }
}

#[test]
fn rejects_wrong_version_truncation_trailing_bytes_and_integrity_failure() {
    let valid = encode_recovery(&record()).unwrap();
    let mut version = valid.clone(); version[8..10].copy_from_slice(&3u16.to_le_bytes()); resign(&mut version);
    assert!(decode_recovery(&version, identity(), None, 0).unwrap_err().contains("version"));
    assert!(decode_recovery(&valid[..valid.len() - 1], identity(), None, 0).unwrap_err().contains("length"));
    let mut trailing = valid.clone(); trailing.push(0); assert!(decode_recovery(&trailing, identity(), None, 0).unwrap_err().contains("length"));
    let mut corrupt = valid.clone(); corrupt[78] ^= 1; assert!(decode_recovery(&corrupt, identity(), None, 0).unwrap_err().contains("integrity"));
}

#[test]
fn reads_existing_v1_active_snapshot_strictly() {
    let expected = record();
    let mut bytes = encode_recovery(&expected).unwrap();
    bytes[8..10].copy_from_slice(&1u16.to_le_bytes());
    resign(&mut bytes);
    assert_eq!(decode_recovery(&bytes, identity(), None, 0).unwrap(), expected);
}

#[test]
fn round_trips_v2_inactive_tombstone_without_edit_state() {
    let tombstone = RecoveryRecord { active: false, generation: 8, revision: 4, current_page: 0, source_pages: 3, source: identity(), edit_state: RecoveryEditState { pages: Vec::new() } };
    let bytes = encode_recovery(&tombstone).unwrap();
    assert_eq!(decode_recovery(&bytes, identity(), Some(7), 4).unwrap(), tombstone);
    let mut invalid = tombstone.clone(); invalid.current_page = 1;
    assert!(encode_recovery(&invalid).unwrap_err().contains("tombstone"));
}

#[test]
fn rejects_stale_source_generation_revision_and_noncanonical_payload() {
    let valid = encode_recovery(&record()).unwrap();
    assert!(decode_recovery(&valid, SourceIdentity { bytes: 4097, ..identity() }, None, 0).unwrap_err().contains("source"));
    assert!(decode_recovery(&valid, identity(), Some(7), 0).unwrap_err().contains("generation"));
    assert!(decode_recovery(&valid, identity(), None, 5).unwrap_err().contains("revision"));
    let mut noncanonical = valid.clone();
    let payload_len = u32::from_le_bytes(noncanonical[74..78].try_into().unwrap()) as usize;
    let payload = &noncanonical[78..78 + payload_len];
    let altered = [b" ".as_slice(), payload].concat();
    noncanonical.truncate(74); noncanonical.extend_from_slice(&(altered.len() as u32).to_le_bytes()); noncanonical.extend_from_slice(&altered); let digest = Sha256::digest(&noncanonical); noncanonical.extend_from_slice(&digest);
    assert!(decode_recovery(&noncanonical, identity(), None, 0).unwrap_err().contains("canonical"));
}

#[test]
fn rejects_invalid_page_annotation_geometry_and_bounds_before_encoding() {
    let mut duplicate = record(); duplicate.edit_state.pages[1].source = 2; assert!(encode_recovery(&duplicate).unwrap_err().contains("page state"));
    let mut turns = record(); turns.edit_state.pages[0].turns = 4; assert!(encode_recovery(&turns).unwrap_err().contains("page state"));
    let mut geometry = record(); geometry.edit_state.pages[0].annotations[0].rect.right = f32::NAN; assert!(encode_recovery(&geometry).unwrap_err().contains("annotation state"));
    let mut wrong_quads = record(); wrong_quads.edit_state.pages[0].annotations[0].quads.push(rect(1.0)); assert!(encode_recovery(&wrong_quads).unwrap_err().contains("annotation state"));
    let mut text = record(); text.edit_state.pages[0].annotations[0].contents = "x".repeat(8193); assert!(encode_recovery(&text).unwrap_err().contains("state"));
    let mut page = record(); page.current_page = 2; assert!(encode_recovery(&page).unwrap_err().contains("page count"));
}

#[test]
fn mirrors_owned_annotation_ids_bodies_geometry_and_global_uniqueness() {
    let mut duplicate = record(); duplicate.edit_state.pages[1].annotations[0].id = duplicate.edit_state.pages[0].annotations[0].id.clone();
    let mut empty_note = record(); empty_note.edit_state.pages[0].annotations[0].contents = "  \n".into();
    let mut wrong_prefix = record(); wrong_prefix.edit_state.pages[0].annotations[0].id = "pdf-workstation-highlight-1".into();
    let mut leading_zero = record(); leading_zero.edit_state.pages[1].annotations[0].id = "pdf-workstation-highlight-02".into();
    let mut wrong_union = record(); wrong_union.edit_state.pages[1].annotations[0].rect.right += 1.0;
    let mut whitespace_highlight = record(); whitespace_highlight.edit_state.pages[1].annotations[0].contents = " \t".into();
    for hostile in [duplicate, empty_note, wrong_prefix, leading_zero, wrong_union, whitespace_highlight] {
        assert!(encode_recovery(&hostile).unwrap_err().contains("annotation state"));
        let frame = frame_with_state(&hostile.edit_state);
        assert!(decode_recovery(&frame, identity(), None, 0).unwrap_err().contains("annotation state"));
    }
}
