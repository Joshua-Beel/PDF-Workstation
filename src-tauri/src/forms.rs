use crate::editor::EditSession;
use lopdf::{content::{Content, Operation}, dictionary, xref::XrefEntry, Dictionary, Document, LoadOptions, Object, ObjectId, Stream};
use serde::{ser::SerializeMap, Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const MAX_FIELDS: usize = 256;
const MAX_SOURCE: usize = 64 * 1024 * 1024;
const MAX_OUTPUT: usize = 256 * 1024 * 1024;
const MAX_VALUE: usize = 4096;
const MAX_TEXT: usize = 1024 * 1024;
const MAX_AP: usize = 64 * 1024;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FormFields {
    pub document_id: u64, pub revision: u64, pub status: &'static str, pub reason: Option<String>,
    pub input: &'static str, pub value_byte_limit: usize, pub fields: Vec<FormField>,
}

#[cfg(test)]
mod tests {
    use super::*;
    const FIXTURE: &[u8] = include_bytes!("../tests/fixtures/reportlab-plain-fields.pdf");
    const MIXED: &[u8] = include_bytes!("../tests/fixtures/reportlab-mixed-fields.pdf");
    const RADIO: &[u8] = include_bytes!("../tests/fixtures/reportlab-radio-fields.pdf");
    const CHOICE:&[u8]=include_bytes!("../tests/fixtures/reportlab-choice-fields.pdf");
    fn session(bytes: Vec<u8>) -> EditSession { EditSession::new(bytes, 1) }
    fn fixture() -> EditSession { session(FIXTURE.to_vec()) }
    fn mutated(change: impl FnOnce(&mut Document)) -> EditSession {
        let mut document = Document::load_mem(FIXTURE).unwrap(); change(&mut document);
        let mut bytes = Vec::new(); document.save_to(&mut bytes).unwrap(); session(bytes)
    }
    fn normal_appearances(document: &Document, fields: &[ObjectId]) -> Vec<ObjectId> {
        let mut copy = Document::with_version("1.7"); copy.objects = document.objects.clone();
        copy.trailer.set("Replaced", fields.iter().filter_map(|id| dict(document, document.get_dictionary(*id).ok()?.get(b"AP").ok()?).ok()?.get(b"N").ok()?.as_reference().ok()).map(Object::Reference).collect::<Vec<_>>());
        reachable(&copy).into_iter().collect()
    }
    fn field_ids(document: &Document) -> Vec<ObjectId> {
        dict(document, document.catalog().unwrap().get(b"AcroForm").unwrap()).unwrap().get(b"Fields").unwrap().as_array().unwrap().iter().map(|value| value.as_reference().unwrap()).collect()
    }
    #[test]
    fn choices_tagged_display_export_mapping_defaults_and_unchanged_appearance_are_exact() {
        let source=session(CHOICE.to_vec());let parsed=FormDocument::parse(&source).unwrap();let original=parsed.document.clone();let field=&parsed.fields[0].dto;let choice=field.choice.as_ref().unwrap();assert_eq!(choice.presentation,"dropdown");assert_eq!(field.value,"south-002");let wire=serde_json::to_value(FormDocument::query(&source,9)).unwrap();assert_eq!(wire["fields"][0]["kind"],"choice");assert_eq!(wire["fields"][0]["presentation"],"dropdown");assert_eq!(wire["fields"][0]["options"][0]["label"],"North Hub");assert!(wire["fields"][0].get("value").is_none()&&wire["fields"][0]["options"][0].get("export").is_none());let patch:FieldValue=serde_json::from_value(serde_json::json!({"kind":"choice","fieldId":field.field_id,"optionId":choice.options[0].option_id})).unwrap();assert!(matches!(serde_json::from_value::<FieldValue>(serde_json::json!({"kind":"choice","fieldId":field.field_id,"optionId":null})).unwrap(),FieldValue::Choice {option_id:None,..}));assert!(serde_json::from_value::<FieldValue>(serde_json::json!({"kind":"choice","fieldId":field.field_id})).is_err());assert!(serde_json::from_value::<FieldValue>(serde_json::json!({"kind":"choice","fieldId":field.field_id,"optionId":choice.options[0].option_id,"value":"forged"})).is_err());
        let(bytes,fields)=parsed.prepare(&[patch]).unwrap();assert_eq!(fields[0].value,"north-001");let output=FormDocument::parse(&session(bytes)).unwrap();let ids=field_ids(&original);let changed=output.document.get_dictionary(ids[0]).unwrap();assert_eq!(text(changed.get(b"V").unwrap()).unwrap(),"north-001");assert_eq!(changed.get(b"I").unwrap().as_array().unwrap(),&vec![Object::Integer(0)]);for key in[b"Opt".as_slice(),b"DV",b"BS",b"MK",b"Rect",b"DA"]{assert_eq!(changed.get(key).unwrap(),original.get_dictionary(ids[0]).unwrap().get(key).unwrap());}assert_eq!(output.document.get_object(ids[1]).unwrap(),original.get_object(ids[1]).unwrap());let replaced=normal_appearances(&original,&ids[..1]);for(id,value)in&original.objects{if replaced.contains(id){if let Ok(kept)=output.document.get_object(*id){assert_eq!(kept,value);}continue;}assert_eq!(output.document.get_object(*id).unwrap(),if *id==ids[0]{output.document.get_object(*id).unwrap()}else{value});}assert_eq!(source.source,CHOICE);
    }
    #[test]
    fn choices_unchanged_patches_keep_original_appearance_and_blank_state() {
        let source=session(CHOICE.to_vec());let parsed=FormDocument::parse(&source).unwrap();let original=parsed.document.clone();let ids=field_ids(&original);let patches=parsed.fields.iter().enumerate().map(|(index,field)|FieldValue::Choice {field_id:field.dto.field_id.clone(),option_id:Some(field.dto.choice.as_ref().unwrap().options[if index==0{1}else{0}].option_id.clone())}).collect::<Vec<_>>();let(bytes,_)=parsed.prepare(&patches).unwrap();let output=Document::load_mem(&bytes).unwrap();assert_eq!(output.get_object(ids[0]).unwrap(),original.get_object(ids[0]).unwrap(),"An unchanged submitted field must retain its original AP reference and state");
        let mut blank=Document::load_mem(include_bytes!("../tests/fixtures/reportlab-choice-blank-fields.pdf")).unwrap();let ids=field_ids(&blank);blank.get_dictionary_mut(ids[0]).unwrap().remove(b"V");blank.get_dictionary_mut(ids[0]).unwrap().set("I",Vec::<Object>::new());let mut bytes=Vec::new();blank.save_to(&mut bytes).unwrap();let source=session(bytes.clone());let parsed=FormDocument::parse(&source).unwrap();let patch=FieldValue::Choice {field_id:parsed.fields[1].dto.field_id.clone(),option_id:Some(parsed.fields[1].dto.choice.as_ref().unwrap().options[0].option_id.clone())};let(output,_)=parsed.prepare(&[patch]).unwrap();let output=Document::load_mem(&output).unwrap();assert_eq!(output.get_object(ids[0]).unwrap(),blank.get_object(ids[0]).unwrap());assert_eq!(source.source,bytes);
    }
    #[test]
    fn choices_mixed_kinds_unchanged_patches_validate_and_preserve_original_objects() {
        let mut doc=Document::load_mem(CHOICE).unwrap();let pages=doc.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap();let form=doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap();let mut roots=field_ids(&doc).into_iter().map(Object::Reference).collect::<Vec<_>>();for fixture in[MIXED,RADIO]{let mut extra=Document::load_mem(fixture).unwrap();extra.renumber_objects_with(doc.max_id+1);let field_roots=field_ids(&extra);roots.extend(field_roots.into_iter().map(Object::Reference));let page=*extra.get_pages().values().next().unwrap();extra.get_dictionary_mut(page).unwrap().set("Parent",pages);doc.objects.extend(extra.objects);doc.max_id=doc.objects.keys().map(|id|id.0).max().unwrap();doc.get_dictionary_mut(pages).unwrap().get_mut(b"Kids").unwrap().as_array_mut().unwrap().push(Object::Reference(page));}doc.get_dictionary_mut(pages).unwrap().set("Count",3);doc.get_dictionary_mut(form).unwrap().set("Fields",roots);let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();let source=EditSession::new(bytes.clone(),3);let parsed=FormDocument::parse(&source).unwrap();let original=parsed.document.clone();assert_eq!(parsed.fields.len(),5);let mut patches=Vec::new();let mut changed_id=None;for field in &parsed.fields{let dto=&field.dto;patches.push(if let Some(choice)=&dto.choice{let index=if choice.presentation=="list"{changed_id=Some(field.object);0}else{1};FieldValue::Choice {field_id:dto.field_id.clone(),option_id:Some(choice.options[index].option_id.clone())}}else if let Some(radio)=&dto.radio{FieldValue::Radio {field_id:dto.field_id.clone(),option_id:radio.selected_option_id.clone()}}else if let Some(checked)=dto.checked{FieldValue::Checkbox {field_id:dto.field_id.clone(),checked}}else{FieldValue::Text {field_id:dto.field_id.clone(),value:dto.value.clone()}});}let(output,_)=parsed.prepare(&patches).unwrap();let output=Document::load_mem(&output).unwrap();let replaced=normal_appearances(&original,&changed_id.into_iter().collect::<Vec<_>>());let live=reachable(&original);for(id,value)in&original.objects{if !live.contains(id){assert!(output.get_object(*id).is_err(),"Objects unreachable in the source are not carried into the copy");continue;}if replaced.contains(id){if let Ok(kept)=output.get_object(*id){assert_eq!(kept,value);}continue;}if Some(*id)!=changed_id{assert_eq!(output.get_object(*id).unwrap(),value,"Only the semantically changed field may alter an original object");}}
        let fields=FormDocument::parse(&source).unwrap().fields;let changed=patches[1].clone();for field in &fields{let dto=&field.dto;let wrong=if dto.checked.is_some(){FieldValue::Choice {field_id:dto.field_id.clone(),option_id:Some("forged".into())}}else if dto.radio.is_some(){FieldValue::Text {field_id:dto.field_id.clone(),value:dto.value.clone()}}else if dto.choice.is_some(){FieldValue::Checkbox {field_id:dto.field_id.clone(),checked:false}}else{FieldValue::Radio {field_id:dto.field_id.clone(),option_id:Some("forged".into())}};assert!(FormDocument::parse(&source).unwrap().prepare(&[changed.clone(),wrong]).is_err());}assert!(FormDocument::parse(&source).unwrap().prepare(&[changed.clone(),patches[0].clone(),patches[0].clone()]).is_err());assert!(FormDocument::parse(&source).unwrap().prepare(&[changed,FieldValue::Choice {field_id:"unknown".into(),option_id:Some("unknown".into())}]).is_err());assert_eq!(source.source,bytes);
    }
    #[test]
    fn choices_options_budget_all_labels_fit_strings_and_unknown_appearances_refuse() {
        let build=|count:usize|{let mut doc=Document::load_mem(CHOICE).unwrap();let original=field_ids(&doc)[0];let field=doc.get_dictionary(original).unwrap().clone();let page=*doc.get_pages().values().next().unwrap();let form=doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap();let mut refs=Vec::new();for index in 0..count{let mut field=field.clone();field.set("T",Object::string_literal(format!("Choice {index}")));let mut options=field.get(b"Opt").unwrap().as_array().unwrap().clone();options.extend((3..32).map(|option|Object::string_literal(format!("Option {option}"))));field.set("Opt",options);refs.push(Object::Reference(doc.add_object(field)));}doc.get_dictionary_mut(form).unwrap().set("Fields",refs.clone());doc.get_dictionary_mut(page).unwrap().set("Annots",refs);let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();session(bytes)};assert_eq!(FormDocument::parse(&build(8)).unwrap().fields.iter().map(|field|field.dto.choice.as_ref().unwrap().options.len()).sum::<usize>(),256);assert_eq!(FormDocument::parse(&build(9)).err().unwrap(),"Form filling is limited to 256 radio and choice options in total.");
        let mut doc=Document::load_mem(CHOICE).unwrap();for id in field_ids(&doc){let field=doc.get_dictionary_mut(id).unwrap();let options=field.get(b"Opt").unwrap().as_array().unwrap().iter().map(|option|option.as_array().unwrap()[1].clone()).collect::<Vec<_>>();let value=options[1].clone();field.set("Opt",options);field.set("V",value.clone());field.set("DV",value);}let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();assert_eq!(FormDocument::parse(&session(bytes)).unwrap().fields[0].dto.value,"South Hub");
        for kind in 0..4{let mut doc=Document::load_mem(CHOICE).unwrap();let ids=field_ids(&doc);match kind{0=>{doc.get_dictionary_mut(ids[0]).unwrap().get_mut(b"Opt").unwrap().as_array_mut().unwrap()[0]=Object::Array(vec![Object::string_literal("north-001"),Object::string_literal("W".repeat(80))]);},1=>{doc.get_dictionary_mut(ids[1]).unwrap().set("Rect",vec![60.into(),504.into(),280.into(),544.into()]);},2=>{doc.get_dictionary_mut(ids[1]).unwrap().remove(b"AP");},_=>{let normal=doc.get_dictionary(ids[0]).unwrap().get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap();let stream=doc.get_object_mut(normal).unwrap().as_stream_mut().unwrap();let content=stream.decompressed_content_with_limit(MAX_AP).unwrap();stream.dict.remove(b"Filter");stream.set_content(String::from_utf8(content).unwrap().replace("South Hub","Forged").into_bytes());}}let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();assert_eq!(FormDocument::query(&session(bytes),1).status,"unsupported","Choice layout case {kind}");}
    }
    #[test]
    fn choices_refuse_unsafe_flags_ambiguous_options_indices_styles_and_patch_kinds() {
        for kind in 0..17{let mut doc=Document::load_mem(CHOICE).unwrap();let ids=field_ids(&doc);let field=doc.get_dictionary_mut(ids[0]).unwrap();match kind{0=>field.set("Ff",131072|262144|524288),1=>field.set("Ff",131072|2097152),2=>field.set("Ff",131072|524288),3=>field.set("Ff",131073),4=>field.set("I",vec![Object::Integer(0)]),5=>field.set("I",vec![Object::Integer(1),Object::Integer(2)]),6=>field.set("TI",1),7=>field.set("V",Object::string_literal("unknown")),8=>field.set("DV",Object::string_literal("unknown")),9=>field.set("Opt",vec![Object::string_literal("duplicate");2]),10=>field.set("Opt",vec![Object::string_literal("A");33]),11=>field.set("Opt",vec![Object::Array(vec![Object::string_literal("export"),Object::string_literal("label"),Object::string_literal("extra")])]),12=>field.set("Q",1),13=>field.set("F",6),14=>field.set("Parent",ids[1]),15=>field.set("AA",dictionary!{}),_=>field.set("Opt",vec![Object::string_literal("é")])}let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();assert_eq!(FormDocument::query(&session(bytes),1).status,"unsupported","Choice adversarial case {kind}");}
        let source=session(CHOICE.to_vec());let field=FormDocument::parse(&source).unwrap().fields[0].dto.clone();for patches in[vec![FieldValue::Text {field_id:field.field_id.clone(),value:"North Hub".into()}],vec![FieldValue::Radio {field_id:field.field_id.clone(),option_id:Some(field.choice.as_ref().unwrap().options[0].option_id.clone())}],vec![FieldValue::Choice {field_id:field.field_id.clone(),option_id:Some("unknown".into())}]]{assert!(FormDocument::parse(&source).unwrap().prepare(&patches).is_err());}assert_eq!(source.source,CHOICE);
    }
    #[test]
    fn choices_clear_removes_value_and_index_and_regenerates_blank_appearance_while_radios_refuse() {
        let source=session(CHOICE.to_vec());let parsed=FormDocument::parse(&source).unwrap();let original=parsed.document.clone();let ids=field_ids(&original);
        assert_eq!(parsed.fields.iter().map(|field|field.dto.choice.as_ref().unwrap().presentation).collect::<Vec<_>>(),vec!["dropdown","list"]);
        let patches=parsed.fields.iter().map(|field|FieldValue::Choice {field_id:field.dto.field_id.clone(),option_id:None}).collect::<Vec<_>>();
        let(bytes,fields)=parsed.prepare(&patches).unwrap();assert!(fields.iter().all(|field|field.value.is_empty()&&field.choice.as_ref().unwrap().selected_option_id.is_none()));
        let output=Document::load_mem(&bytes).unwrap();
        for id in &ids{let field=output.get_dictionary(*id).unwrap();let before=original.get_dictionary(*id).unwrap();assert!(field.get(b"V").is_err()&&field.get(b"I").is_err(),"A cleared choice must write neither /V nor /I");assert_ne!(field.get(b"AP").unwrap(),before.get(b"AP").unwrap());for key in[b"Opt".as_slice(),b"DV",b"DA",b"Ff",b"Rect",b"MK",b"BS"]{assert_eq!(field.get(key).ok(),before.get(key).ok());}}
        let replaced=normal_appearances(&original,&ids);for(id,value)in&original.objects{if replaced.contains(id){if let Ok(kept)=output.get_object(*id){assert_eq!(kept,value);}continue;}if !ids.contains(id){assert_eq!(output.get_object(*id).unwrap(),value);}}
        let reparsed=FormDocument::parse(&session(bytes.clone())).unwrap();
        for field in &reparsed.fields{let choice=field.dto.choice.as_ref().unwrap();assert!(choice.selected_option_id.is_none());let normal=reparsed.document.get_dictionary(field.object).unwrap().get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_reference().unwrap();let stream=reparsed.document.get_object(normal).unwrap().as_stream().unwrap();let content=stream.decompressed_content().unwrap_or_else(|_|stream.content.clone());assert_eq!(content,choice_appearance(field.style.as_ref().unwrap(),choice).unwrap().encode().unwrap(),"Cleared {} appearance must be the regenerated blank appearance",choice.presentation);}
        assert_eq!(reparsed.prepare(&patches).err().unwrap(),"Change at least one form value before saving a new copy.");
        let blank=session(include_bytes!("../tests/fixtures/reportlab-choice-blank-fields.pdf").to_vec());let parsed=FormDocument::parse(&blank).unwrap();let mixed=vec![FieldValue::Choice {field_id:parsed.fields[0].dto.field_id.clone(),option_id:Some(parsed.fields[0].dto.choice.as_ref().unwrap().options[1].option_id.clone())},FieldValue::Choice {field_id:parsed.fields[1].dto.field_id.clone(),option_id:None}];let expected=parsed.fields[0].dto.choice.as_ref().unwrap().options[1].export.clone();let(_,fields)=parsed.prepare(&mixed).unwrap();assert_eq!(fields[0].value,expected);assert!(fields[1].value.is_empty());
        let radio=session(RADIO.to_vec());let field_id=FormDocument::parse(&radio).unwrap().fields[0].dto.field_id.clone();let patch:FieldValue=serde_json::from_value(serde_json::json!({"kind":"radio","fieldId":field_id,"optionId":null})).unwrap();assert!(matches!(patch,FieldValue::Radio {option_id:None,..}));
        assert_eq!(FormDocument::parse(&radio).unwrap().prepare(&[patch.clone()]).err().unwrap(),"A radio group selection cannot be cleared: the form author requires one option to stay selected.");
        let mut doc=Document::load_mem(RADIO).unwrap();let parent=field_ids(&doc)[0];let kids=doc.get_dictionary(parent).unwrap().get(b"Kids").unwrap().as_array().unwrap().clone();doc.get_dictionary_mut(parent).unwrap().remove(b"V");for kid in kids{doc.get_dictionary_mut(kid.as_reference().unwrap()).unwrap().set("AS",Object::Name(b"Off".to_vec()));}let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();assert!(FormDocument::parse(&session(bytes.clone())).unwrap().prepare(&[patch]).is_err(),"A blank radio group must still refuse a null selection");assert_eq!(radio.source,RADIO);
    }
    #[test]
    fn radios_tagged_selected_blank_source_switching_and_raw_appearances_are_exact() {
        let source=session(RADIO.to_vec());let parsed=FormDocument::parse(&source).unwrap();assert_eq!(parsed.fields.len(),1);let field=&parsed.fields[0].dto;let radio=field.radio.as_ref().unwrap();assert_eq!(radio.options.iter().map(|option|option.label.as_str()).collect::<Vec<_>>(),vec!["Choice A","Choice B","Choice C"]);assert_eq!(radio.selected_option_id,Some(radio.options[1].option_id.clone()));
        let wire=serde_json::to_value(FormDocument::query(&source,42)).unwrap();let item=&wire["fields"][0];assert_eq!(item["kind"],"radio");assert_eq!(item["fieldId"],field.field_id);assert_eq!(item["selectedOptionId"],radio.options[1].option_id);assert_eq!(item["options"][1]["label"],"Choice B");assert!(item.get("value").is_none()&&item.get("checked").is_none()&&item["options"][0].get("object").is_none());
        let patch:FieldValue=serde_json::from_value(serde_json::json!({"fieldId":field.field_id,"kind":"radio","optionId":radio.options[0].option_id})).unwrap();for bad in[serde_json::json!({"fieldId":field.field_id,"kind":"radio"}),serde_json::json!({"fieldId":field.field_id,"kind":"radio","optionId":radio.options[0].option_id,"checked":true})]{assert!(serde_json::from_value::<FieldValue>(bad).is_err());}
        let original=parsed.document.clone();let(bytes,fields)=parsed.prepare(&[patch]).unwrap();let changed=FormDocument::parse(&session(bytes)).unwrap();assert_eq!(fields[0].value,"Choice A");let parent=changed.document.get_dictionary(changed.fields[0].object).unwrap();assert_eq!(parent.get(b"V").unwrap().as_name().unwrap(),b"Choice A");
        for(index,option)in changed.fields[0].dto.radio.as_ref().unwrap().options.iter().enumerate(){let widget=changed.document.get_dictionary(option.object).unwrap();assert_eq!(widget.get(b"AS").unwrap().as_name().unwrap(),if index==0{b"Choice A".as_slice()}else{b"Off".as_slice()});for key in[b"AP".as_slice(),b"MK",b"BS",b"Rect",b"F",b"Parent",b"P"]{assert_eq!(widget.get(key).ok(),original.get_dictionary(option.object).unwrap().get(key).ok());}}
        for(id,value)in&original.objects{if matches!(value,Object::Stream(_)){assert_eq!(changed.document.get_object(*id).unwrap(),value,"All original raw AP streams/resources remain unchanged");}}
        let mut blank=original;let parent_id=field_ids(&blank)[0];let children=blank.get_dictionary(parent_id).unwrap().get(b"Kids").unwrap().as_array().unwrap().clone();blank.get_dictionary_mut(parent_id).unwrap().remove(b"V");for child in children{blank.get_dictionary_mut(child.as_reference().unwrap()).unwrap().set("AS",Object::Name(b"Off".to_vec()));}let mut bytes=Vec::new();blank.save_to(&mut bytes).unwrap();let blank=session(bytes);let parsed=FormDocument::parse(&blank).unwrap();assert!(parsed.fields[0].dto.radio.as_ref().unwrap().selected_option_id.is_none());assert!(serde_json::to_value(FormDocument::query(&blank,1)).unwrap()["fields"][0]["selectedOptionId"].is_null());let radio=parsed.fields[0].dto.radio.as_ref().unwrap();let patch=FieldValue::Radio {field_id:parsed.fields[0].dto.field_id.clone(),option_id:Some(radio.options[2].option_id.clone())};assert_eq!(parsed.prepare(&[patch]).unwrap().1[0].value,"Choice C");assert_eq!(source.source,RADIO);
    }
    #[test]
    fn radios_refuse_ambiguous_hierarchies_states_flags_pages_and_transforms() {
        for kind in 0..21 {
            let mut doc=Document::load_mem(RADIO).unwrap();let parent=field_ids(&doc)[0];let kids=doc.get_dictionary(parent).unwrap().get(b"Kids").unwrap().as_array().unwrap().clone();let a=kids[0].as_reference().unwrap();let b=kids[1].as_reference().unwrap();let page=*doc.get_pages().values().next().unwrap();
            match kind {
                0=>{doc.get_dictionary_mut(parent).unwrap().set("Kids",vec![kids[0].clone(),kids[0].clone()]);},
                1=>{doc.get_dictionary_mut(parent).unwrap().set("Kids",vec![Object::Reference(parent),kids[1].clone()]);},
                2=>{doc.get_dictionary_mut(a).unwrap().set("Parent",a);},
                3=>{doc.get_dictionary_mut(a).unwrap().set("T",Object::string_literal("Ambiguous child name"));},
                4=>{doc.get_dictionary_mut(a).unwrap().set("AS",Object::Name(b"Choice A".to_vec()));},
                5=>{doc.get_dictionary_mut(parent).unwrap().remove(b"V");},
                6=>{doc.get_dictionary_mut(b).unwrap().set("AS",Object::Name(b"Off".to_vec()));},
                7=>{doc.get_dictionary_mut(parent).unwrap().set("V",Object::Name(b"Unknown".to_vec()));},
                8=>{doc.get_dictionary_mut(parent).unwrap().set("DV",Object::Name(b"Unknown".to_vec()));},
                9=>{doc.get_dictionary_mut(parent).unwrap().set("Ff",49154|33554432);},
                10=>{doc.get_dictionary_mut(parent).unwrap().set("Ff",49155);},
                11=>{doc.get_dictionary_mut(a).unwrap().set("F",6);},
                12=>{doc.get_dictionary_mut(a).unwrap().set("P",parent);},
                13=>{doc.get_dictionary_mut(page).unwrap().set("Annots",vec![kids[0].clone(),kids[1].clone()]);},
                14=>{let mut refs=kids.clone();refs.push(kids[0].clone());doc.get_dictionary_mut(page).unwrap().set("Annots",refs);},
                15=>{doc.get_dictionary_mut(parent).unwrap().set("AA",dictionary!{});},
                16=>{doc.get_dictionary_mut(parent).unwrap().set("Kids",vec![kids[0].clone();33]);},
                17=>{let ap=doc.get_dictionary_mut(a).unwrap().get_mut(b"AP").unwrap().as_dict_mut().unwrap();for(_,states)in ap {let states=states.as_dict_mut().unwrap();let on=states.remove(b"Choice A").unwrap();states.set("Choice B",on);}},
                18=>{let ap=doc.get_dictionary(a).unwrap().get(b"AP").unwrap().as_dict().unwrap().get(b"N").unwrap().as_dict().unwrap().get(b"Choice A").unwrap().as_reference().unwrap();let stream=doc.get_object_mut(ap).unwrap().as_stream_mut().unwrap();stream.dict.remove(b"Filter");stream.set_content(b"q 1 0 0 1 11 10 cm 0 0 1 1 re f Q".to_vec());},
                19=>{let form=doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap();let mut second=doc.get_dictionary(parent).unwrap().clone();second.set("T",Object::string_literal("Shared children"));let second=doc.add_object(second);doc.get_dictionary_mut(form).unwrap().set("Fields",vec![Object::Reference(parent),Object::Reference(second)]);},
                _=>{doc.get_dictionary_mut(a).unwrap().set("Ff",49154);},
            }
            let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();let source=session(bytes.clone());let query=FormDocument::query(&source,7);assert_eq!(query.status,"unsupported","Radio adversarial case {kind}");assert!(query.fields.is_empty());assert_eq!(source.source,bytes);
        }
        let source=session(RADIO.to_vec());let field=FormDocument::parse(&source).unwrap().fields[0].dto.clone();for patches in[vec![FieldValue::Radio {field_id:field.field_id.clone(),option_id:Some("unknown".into())}],vec![FieldValue::Radio {field_id:field.field_id.clone(),option_id:Some(String::new())}],vec![FieldValue::Radio {field_id:field.field_id.clone(),option_id:None}],vec![FieldValue::Checkbox {field_id:field.field_id.clone(),checked:false}],vec![FieldValue::Text {field_id:field.field_id.clone(),value:"Choice A".into()}],vec![FieldValue::Radio {field_id:field.field_id.clone(),option_id:Some(field.radio.as_ref().unwrap().options[1].option_id.clone())}]] {assert!(FormDocument::parse(&source).unwrap().prepare(&patches).is_err());}
    }
    #[test]
    fn radios_widget_budget_defaults_custom_exact_labels_and_cross_kind_patches_are_atomic() {
        let build=|groups:usize|{let mut doc=Document::load_mem(RADIO).unwrap();let form=doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap();let original=field_ids(&doc)[0];let parent=doc.get_dictionary(original).unwrap().clone();let child=doc.get_dictionary(parent.get(b"Kids").unwrap().as_array().unwrap()[0].as_reference().unwrap()).unwrap().clone();let page=*doc.get_pages().values().next().unwrap();let mut roots=Vec::new();let mut widgets=Vec::new();for group in 0..groups{let mut root=parent.clone();root.set("T",Object::string_literal(format!("Group {group}")));root.remove(b"V");let root=doc.add_object(root);let mut kids=Vec::new();for index in 0..32{let mut widget=child.clone();widget.set("Parent",root);let mut ap=widget.get(b"AP").unwrap().as_dict().unwrap().clone();for(_,states)in&mut ap{let states=states.as_dict_mut().unwrap();let on=states.remove(b"Choice A").unwrap();states.set(format!("Choice {index}"),on);}widget.set("AP",ap);widget.set("AS",Object::Name(b"Off".to_vec()));let id=doc.add_object(widget);kids.push(Object::Reference(id));widgets.push(Object::Reference(id));}doc.get_dictionary_mut(root).unwrap().set("Kids",kids);roots.push(Object::Reference(root));}doc.get_dictionary_mut(form).unwrap().set("Fields",roots);doc.get_dictionary_mut(page).unwrap().set("Annots",widgets);let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();session(bytes)};
        let boundary=build(8);let parsed=FormDocument::parse(&boundary).unwrap();assert_eq!(parsed.fields.len(),8);let untouched=parsed.document.clone();let first=&parsed.fields[0].dto;let patch=FieldValue::Radio {field_id:first.field_id.clone(),option_id:Some(first.radio.as_ref().unwrap().options[0].option_id.clone())};let(output,fields)=parsed.prepare(&[patch]).unwrap();assert!(fields[1..].iter().all(|field|field.radio.as_ref().unwrap().selected_option_id.is_none()));let output=FormDocument::parse(&session(output)).unwrap();for field in &output.fields[1..]{assert_eq!(output.document.get_object(field.object).unwrap(),untouched.get_object(field.object).unwrap());for option in &field.dto.radio.as_ref().unwrap().options{assert_eq!(output.document.get_object(option.object).unwrap(),untouched.get_object(option.object).unwrap());}}
        let oversized=build(9);let mut doc=Document::load_mem(&oversized.source).unwrap();let page=*doc.get_pages().values().next().unwrap();let pages=doc.get_dictionary(page).unwrap().get(b"Parent").unwrap().as_reference().unwrap();let mut extra=doc.get_dictionary(page).unwrap().clone();let widgets=extra.get(b"Annots").unwrap().as_array().unwrap().clone();extra.set("Annots",widgets[256..].to_vec());doc.get_dictionary_mut(page).unwrap().set("Annots",widgets[..256].to_vec());let extra=doc.add_object(extra);for widget in &widgets[256..]{doc.get_dictionary_mut(widget.as_reference().unwrap()).unwrap().set("P",extra);}let tree=doc.get_dictionary_mut(pages).unwrap();tree.get_mut(b"Kids").unwrap().as_array_mut().unwrap().push(Object::Reference(extra));tree.set("Count",2);let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();let original_bytes=bytes.clone();let aggregate=EditSession::new(bytes,2);let reason=FormDocument::parse(&aggregate).err().unwrap();assert_eq!(reason,"Form filling is limited to 256 total page widgets.");assert_eq!(aggregate.source,original_bytes);
        let mut doc=Document::load_mem(RADIO).unwrap();let parent=field_ids(&doc)[0];let child=doc.get_dictionary(parent).unwrap().get(b"Kids").unwrap().as_array().unwrap()[0].as_reference().unwrap();let label=" Choice /A (custom) ";let mut ap=doc.get_dictionary(child).unwrap().get(b"AP").unwrap().as_dict().unwrap().clone();for(_,states)in&mut ap{let states=states.as_dict_mut().unwrap();let on=states.remove(b"Choice A").unwrap();states.set(label,on);}doc.get_dictionary_mut(child).unwrap().set("AP",ap);doc.get_dictionary_mut(parent).unwrap().set("DV",Object::Name(b"Choice C".to_vec()));let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();let source=session(bytes.clone());let parsed=FormDocument::parse(&source).unwrap();let field=&parsed.fields[0].dto;let option=field.radio.as_ref().unwrap().options[0].clone();assert_eq!(option.label,label);let id=field.field_id.clone();assert!(FormDocument::parse(&source).unwrap().prepare(&[FieldValue::Radio {field_id:id.clone(),option_id:Some(option.option_id.clone())},FieldValue::Radio {field_id:id.clone(),option_id:Some(option.option_id.clone())}]).is_err());let(output,fields)=parsed.prepare(&[FieldValue::Radio {field_id:id,option_id:Some(option.option_id)}]).unwrap();assert_eq!(fields[0].value,label);let output=FormDocument::parse(&session(output)).unwrap();assert_eq!(output.document.get_dictionary(parent).unwrap().get(b"DV").unwrap().as_name().unwrap(),b"Choice C");assert_eq!(source.source,bytes);
        let mixed=session(MIXED.to_vec());let fields=FormDocument::parse(&mixed).unwrap();for field in fields.fields{assert!(FormDocument::parse(&mixed).unwrap().prepare(&[FieldValue::Radio {field_id:field.dto.field_id,option_id:Some("option".into())}]).is_err());}
    }
    #[test]
    fn checkboxes_tagged_wire_shape_false_and_wrong_kind_are_explicit() {
        let parsed=FormDocument::parse(&session(MIXED.to_vec())).unwrap();
        let wire=serde_json::to_value(FormDocument::query(&session(MIXED.to_vec()),17)).unwrap();
        assert_eq!(wire["documentId"],17); assert_eq!(wire["fields"][0]["kind"],"checkbox");assert_eq!(wire["fields"][0]["checked"],false);
        assert!(wire["fields"][0].get("value").is_none());assert!(wire["fields"][0].get("maxLength").is_none());assert!(wire["fields"][0].get("fieldId").is_some());
        assert_eq!(wire["fields"][1]["kind"],"text");assert_eq!(wire["fields"][1]["value"],"Original");assert_eq!(wire["fields"][1]["maxLength"],40);assert!(wire["fields"][1].get("checked").is_none());
        let id=parsed.fields[0].dto.field_id.clone();
        let false_patch:FieldValue=serde_json::from_value(serde_json::json!({"fieldId":id,"kind":"checkbox","checked":false})).unwrap();
        assert!(matches!(false_patch,FieldValue::Checkbox {checked:false,..}));
        for bad in [serde_json::json!({"fieldId":id,"kind":"checkbox","checked":false,"value":""}),serde_json::json!({"fieldId":id,"kind":"checkbox","checked":"false"}),serde_json::json!({"field_id":id,"kind":"checkbox","checked":true}),serde_json::json!({"fieldId":id,"value":"new"}),serde_json::json!({"fieldId":id,"kind":"text","value":"new","checked":true})] {assert!(serde_json::from_value::<FieldValue>(bad).is_err());}
        assert!(parsed.prepare(&[FieldValue::Text {field_id:id,value:"new".into()}]).is_err());
    }
    #[test]
    fn checkboxes_custom_on_state_toggle_clear_preserve_every_appearance_byte_and_default() {
        let mut doc=Document::load_mem(MIXED).unwrap();let id=field_ids(&doc)[0];
        let mut ap=dict(&doc,doc.get_dictionary(id).unwrap().get(b"AP").unwrap()).unwrap().clone();
        for (_,states) in &mut ap {let states=states.as_dict_mut().unwrap();let on=states.remove(b"Yes").unwrap();states.set("Approved",on);}
        doc.get_dictionary_mut(id).unwrap().set("AP",ap);doc.get_dictionary_mut(id).unwrap().set("DV",Object::Name(b"Approved".to_vec()));
        let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();let source=session(bytes.clone());
        let parsed=FormDocument::parse(&source).unwrap();let patches=vec![FieldValue::Checkbox {field_id:parsed.fields[0].dto.field_id.clone(),checked:true},FieldValue::Text {field_id:parsed.fields[1].dto.field_id.clone(),value:"Mixed copy".into()}];
        let (filled,fields)=parsed.prepare(&patches).unwrap();assert_eq!(fields[0].checked,Some(true));
        let filled=session(filled);let on=FormDocument::parse(&filled).unwrap();let widget=on.document.get_dictionary(id).unwrap();assert_eq!(widget.get(b"V").unwrap().as_name().unwrap(),b"Approved");assert_eq!(widget.get(b"AS").unwrap(),widget.get(b"V").unwrap());
        for key in [b"AP".as_slice(),b"DV",b"MK",b"BS",b"H",b"Rect",b"F",b"Ff"] {assert_eq!(widget.get(key).ok(),doc.get_dictionary(id).unwrap().get(key).ok());}
        let original_ap=dict(&doc,doc.get_dictionary(id).unwrap().get(b"AP").unwrap()).unwrap();
        for (_,states) in original_ap {for (_,reference) in states.as_dict().unwrap() {let stream=reference.as_reference().unwrap();assert_eq!(doc.get_object(stream).unwrap(),on.document.get_object(stream).unwrap(),"Raw compressed stream, resources and dictionary must remain byte-identical");}}
        let text_id=on.fields[1].dto.field_id.clone();assert!(FormDocument::parse(&filled).unwrap().prepare(&[FieldValue::Checkbox {field_id:text_id,checked:true}]).is_err());
        let (cleared,fields)=on.prepare(&[FieldValue::Checkbox {field_id:fields[0].field_id.clone(),checked:false}]).unwrap();assert_eq!(fields[0].checked,Some(false));
        let off=FormDocument::parse(&session(cleared)).unwrap();let widget=off.document.get_dictionary(id).unwrap();assert_eq!(widget.get(b"V").unwrap().as_name().unwrap(),b"Off");assert_eq!(widget.get(b"AS").unwrap(),widget.get(b"V").unwrap());assert_eq!(widget.get(b"DV").unwrap().as_name().unwrap(),b"Approved");assert_eq!(source.source,bytes);
    }
    #[test]
    fn checkboxes_refuse_ambiguous_states_unsafe_artwork_flags_and_hierarchies() {
        for kind in 0..22 {
            let mut doc=Document::load_mem(MIXED).unwrap();let id=field_ids(&doc)[0];
            let normal=dict(&doc,doc.get_dictionary(id).unwrap().get(b"AP").unwrap()).unwrap().get(b"N").unwrap().as_dict().unwrap().clone();let on=normal.get(b"Yes").unwrap().as_reference().unwrap();
            match kind {
                0=>{doc.get_dictionary_mut(id).unwrap().set("AS",Object::Name(b"Yes".to_vec()));},
                1=>{doc.get_dictionary_mut(id).unwrap().remove(b"V");},
                2=>{doc.get_dictionary_mut(id).unwrap().set("DV",Object::Name(b"Unknown".to_vec()));},
                3=>{let ap=doc.get_dictionary_mut(id).unwrap().get_mut(b"AP").unwrap().as_dict_mut().unwrap();ap.get_mut(b"N").unwrap().as_dict_mut().unwrap().set("Other",on);},
                4=>{let ap=doc.get_dictionary_mut(id).unwrap().get_mut(b"AP").unwrap().as_dict_mut().unwrap();let states=ap.get_mut(b"N").unwrap().as_dict_mut().unwrap();states.set("Yes",states.get(b"Off").unwrap().clone());},
                5=>{doc.get_dictionary_mut(id).unwrap().set("Ff",32768);},
                6=>{doc.get_dictionary_mut(id).unwrap().set("Ff",65536);},
                7=>{doc.get_dictionary_mut(id).unwrap().set("F",6);},
                8=>{doc.get_dictionary_mut(id).unwrap().set("Parent",id);},
                9=>{doc.get_dictionary_mut(id).unwrap().set("Kids",vec![Object::Reference(id)]);},
                10=>{doc.get_object_mut(on).unwrap().as_stream_mut().unwrap().dict.set("Resources",dictionary! {"Font"=>dictionary!{}});},
                11=>{doc.get_object_mut(on).unwrap().as_stream_mut().unwrap().dict.set("Matrix",real(&[1.0,0.0,0.0,1.0,1.0,0.0]));},
                12=>{let stream=doc.get_object_mut(on).unwrap().as_stream_mut().unwrap();stream.dict.remove(b"Filter");stream.set_content(b"q 1 0 0 1 0 0 cm Q".to_vec());},
                13=>{let stream=doc.get_object_mut(on).unwrap().as_stream_mut().unwrap();stream.dict.remove(b"Filter");stream.set_content(b"q q q q q q q q q 0 0 20 20 re f Q Q Q Q Q Q Q Q Q".to_vec());},
                14=>{let stream=doc.get_object_mut(on).unwrap().as_stream_mut().unwrap();stream.dict.remove(b"Filter");stream.set_content("0 g ".repeat(257).into_bytes());},
                15=>{let stream=doc.get_object_mut(on).unwrap().as_stream_mut().unwrap();stream.dict.remove(b"Filter");stream.set_content(b"10001 w 0 0 20 20 re f".to_vec());},
                16=>{let stream=doc.get_object_mut(on).unwrap().as_stream_mut().unwrap();stream.dict.remove(b"Filter");stream.set_content(b"Q 0 0 20 20 re f".to_vec());},
                17=>{let stream=doc.get_object_mut(on).unwrap().as_stream_mut().unwrap();stream.dict.remove(b"Filter");stream.set_content(b"0 0 m 100 0 l h f".to_vec());},
                18=>{let ap=doc.get_dictionary_mut(id).unwrap().get_mut(b"AP").unwrap().as_dict_mut().unwrap();let states=ap.get_mut(b"R").unwrap().as_dict_mut().unwrap();let value=states.remove(b"Yes").unwrap();states.set("Different",value);},
                19=>{doc.get_dictionary_mut(id).unwrap().set("AA",dictionary!{});},
                20=>{doc.get_dictionary_mut(id).unwrap().set("H",Object::Name(b"P".to_vec()));},
                _=>{doc.get_dictionary_mut(id).unwrap().set("Rect",real(&[60.0,650.0,80.0,679.0]));},
            }
            let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();let source=session(bytes.clone());let result=FormDocument::query(&source,17);assert_eq!(result.status,"unsupported","Checkbox adversarial case {kind}");assert!(result.fields.is_empty());assert_eq!(source.source,bytes);assert_eq!(source.revision,0);
        }
        let source=session(MIXED.to_vec());let fields=FormDocument::parse(&source).unwrap();let id=fields.fields[0].dto.field_id.clone();assert!(fields.prepare(&[FieldValue::Checkbox {field_id:id.clone(),checked:true},FieldValue::Checkbox {field_id:id,checked:false}]).is_err());assert_eq!(source.source,MIXED);
    }
    #[test]
    fn checkboxes_share_the_total_field_budget_and_preserve_mixed_defaults() {
        let build=|count:usize|{let mut doc=Document::load_mem(MIXED).unwrap();let ids=field_ids(&doc);let checkbox=doc.get_dictionary(ids[0]).unwrap().clone();let text=doc.get_dictionary(ids[1]).unwrap().clone();let page=*doc.get_pages().values().next().unwrap();let form=doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap();let mut refs=Vec::new();for index in 0..count {let mut field=if index%2==0{checkbox.clone()}else{text.clone()};field.set("T",Object::string_literal(format!("MixedField{index}")));refs.push(Object::Reference(doc.add_object(field)));}doc.get_dictionary_mut(form).unwrap().set("Fields",refs.clone());doc.get_dictionary_mut(page).unwrap().set("Annots",refs);let mut bytes=Vec::new();doc.save_to(&mut bytes).unwrap();session(bytes)};
        let source=build(MAX_FIELDS);let parsed=FormDocument::parse(&source).unwrap();assert_eq!(parsed.fields.len(),MAX_FIELDS);assert_eq!(parsed.fields.iter().filter(|field|field.dto.checked.is_some()).count(),MAX_FIELDS/2);assert!(FormDocument::parse(&build(MAX_FIELDS+1)).err().unwrap().contains("256"));
        let values=parsed.fields.iter().map(|field|if field.dto.checked.is_some(){FieldValue::Checkbox {field_id:field.dto.field_id.clone(),checked:true}}else{FieldValue::Text {field_id:field.dto.field_id.clone(),value:"Changed".into()}}).collect::<Vec<_>>();let(bytes,fields)=parsed.prepare(&values).unwrap();assert_eq!(fields.len(),MAX_FIELDS);assert_eq!(FormDocument::parse(&session(bytes)).unwrap().fields.iter().filter(|field|field.dto.checked==Some(true)).count(),MAX_FIELDS/2);
    }
    #[test]
    fn forms_reportlab_values_appearances_and_styles_round_trip_and_clear() {
        let source = fixture(); let parsed = FormDocument::parse(&source).unwrap();
        assert_eq!(parsed.fields.len(), 2); assert_eq!(parsed.fields[0].dto.value, "Original");
        let literal = "Portland (OR) \\ `city`";
        let patches = parsed.fields.iter().enumerate().map(|(index, field)| FieldValue::Text { field_id: field.dto.field_id.clone(), value: if index == 0 { String::new() } else { literal.into() } }).collect::<Vec<_>>();
        let (bytes, fields) = parsed.prepare(&patches).unwrap();
        let output = session(bytes); let reparsed = FormDocument::parse(&output).unwrap();
        assert_eq!(fields[0].value, ""); assert_eq!(fields[1].value, literal);
        let city = reparsed.document.get_dictionary(reparsed.fields[1].object).unwrap();
        let ap = dict(&reparsed.document, city.get(b"AP").unwrap()).unwrap().get(b"N").unwrap();
        let stream = object(&reparsed.document, ap).unwrap().as_stream().unwrap();
        let decoded = Content::decode(&stream.content).unwrap(); let shown = decoded.operations.iter().filter(|operation| operation.operator == "Tj").collect::<Vec<_>>();
        assert_eq!(shown.len(),1); assert_eq!(shown[0].operands[0].as_str().unwrap(),literal.as_bytes());
        let original = FormDocument::parse(&source).unwrap();
        for (before, after) in original.fields.iter().zip(&reparsed.fields) {
            assert_eq!(before.style.as_ref().unwrap().size, after.style.as_ref().unwrap().size); assert_eq!(before.style.as_ref().unwrap().background, after.style.as_ref().unwrap().background);
            assert_eq!(before.style.as_ref().unwrap().border_color, after.style.as_ref().unwrap().border_color); assert_eq!(before.style.as_ref().unwrap().text_color, after.style.as_ref().unwrap().text_color);
            let a = original.document.get_dictionary(before.object).unwrap(); let b = reparsed.document.get_dictionary(after.object).unwrap();
            for key in [b"DV".as_slice(), b"DA", b"Rect", b"BS", b"MK", b"P", b"T", b"F", b"Ff"] { assert_eq!(a.get(key).ok(), b.get(key).ok(), "Preserve original widget property"); }
        }
        assert_eq!(source.source, FIXTURE); assert_eq!(source.revision, 0);
    }
    #[test]
    fn forms_refuse_ambiguous_widgets_hierarchies_actions_and_unknown_appearances() {
        for kind in 0..13 {
            let source = mutated(|doc| {
                let ids = field_ids(doc); let page = *doc.get_pages().values().next().unwrap();
                match kind {
                    0 => { doc.get_dictionary_mut(page).unwrap().set("Annots", vec![Object::Reference(ids[0]),Object::Reference(ids[0])]); },
                    1 => { doc.get_dictionary_mut(page).unwrap().set("Annots", vec![Object::Reference(ids[0])]); },
                    2 => { doc.get_dictionary_mut(ids[1]).unwrap().set("T", Object::string_literal("Name")); },
                    3 => { doc.get_dictionary_mut(ids[0]).unwrap().set("Parent", ids[0]); },
                    4 => { doc.get_dictionary_mut(ids[0]).unwrap().set("Kids", vec![Object::Reference(ids[0])]); },
                    5 => { doc.get_dictionary_mut(ids[0]).unwrap().set("AA", dictionary!{}); },
                    6 => { doc.get_dictionary_mut(ids[0]).unwrap().set("Ff", 4096); },
                    7 => { doc.get_dictionary_mut(ids[0]).unwrap().set("P", ids[0]); },
                    8 => { doc.get_dictionary_mut(ids[0]).unwrap().remove(b"AP"); },
                    9 => { doc.get_dictionary_mut(ids[0]).unwrap().set("V", Object::string_literal("different")); },
                    10 => { let ap = dict(doc, doc.get_dictionary(ids[0]).unwrap().get(b"AP").unwrap()).unwrap().get(b"N").unwrap().as_reference().unwrap(); doc.get_object_mut(ap).unwrap().as_stream_mut().unwrap().dict.set("Matrix", real(&[1.0,0.0,0.0,1.0,5.0,0.0])); },
                    11 => { let pages = doc.catalog().unwrap().get(b"Pages").unwrap().as_reference().unwrap(); doc.get_dictionary_mut(pages).unwrap().set("Kids", vec![Object::Reference(pages)]); },
                    _ => { let form = doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap(); doc.get_dictionary_mut(form).unwrap().set("NeedAppearances", true); },
                }
            });
            let before = source.source.clone(); let query = FormDocument::query(&source, 7);
            assert_eq!(query.status, "unsupported", "Adversarial case {kind}"); assert!(query.fields.is_empty()); assert_eq!(source.source, before);
        }
    }
    #[test]
    fn forms_patch_validation_is_atomic_and_enforces_ascii_length_and_fit() {
        let source = fixture(); let id = FormDocument::parse(&source).unwrap().fields[0].dto.field_id.clone();
        for values in [vec![], vec![FieldValue::Text { field_id: "unknown".into(), value: "new".into() }], vec![FieldValue::Text { field_id:id.clone(),value:"Original".into() }], vec![FieldValue::Text { field_id:id.clone(),value:"世".into() }], vec![FieldValue::Text { field_id:id.clone(),value:"line\nline".into() }], vec![FieldValue::Text { field_id:id.clone(),value:"W".repeat(40) }], vec![FieldValue::Text { field_id:id.clone(),value:"a".repeat(41) }], vec![FieldValue::Text { field_id:id.clone(),value:"a".repeat(4097) }], vec![FieldValue::Text { field_id:id.clone(),value:"a".into() },FieldValue::Text { field_id:id.clone(),value:"b".into() }]] {
            assert!(FormDocument::parse(&source).unwrap().prepare(&values).is_err()); assert_eq!(source.source, FIXTURE); assert_eq!(source.revision, 0);
        }
        let mut edited = fixture(); edited.plan[0].turns = 1; assert!(FormDocument::parse(&edited).is_err());
        let oversized = session(vec![b' '; MAX_SOURCE+1]); assert!(FormDocument::parse(&oversized).err().unwrap().contains("64 MiB"));
    }
    #[test]
    fn forms_import_budgets_and_font_style_trust_boundary_are_explicit() {
        let build_fields = |count: usize| mutated(|doc| {
            let original = doc.get_dictionary(field_ids(doc)[0]).unwrap().clone(); let page = *doc.get_pages().values().next().unwrap(); let form = doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap(); let mut refs = Vec::new();
            for index in 0..count { let mut field = original.clone(); field.set("T",Object::string_literal(format!("Field{index}"))); refs.push(Object::Reference(doc.add_object(field))); }
            doc.get_dictionary_mut(form).unwrap().set("Fields",refs.clone()); doc.get_dictionary_mut(page).unwrap().set("Annots",refs);
        });
        assert_eq!(FormDocument::parse(&build_fields(MAX_FIELDS)).unwrap().fields.len(),MAX_FIELDS);
        assert!(FormDocument::parse(&build_fields(MAX_FIELDS+1)).err().unwrap().contains("256"));
        for kind in 0..12 {
            let source = mutated(|doc| {
                let id=field_ids(doc)[0]; let form=doc.catalog().unwrap().get(b"AcroForm").unwrap().as_reference().unwrap(); let page=*doc.get_pages().values().next().unwrap();
                match kind {
                    0 => {let font=dict(doc,doc.get_dictionary(form).unwrap().get(b"DR").unwrap()).unwrap().get(b"Font").unwrap().as_dict().unwrap().get(b"Helv").unwrap().as_reference().unwrap(); doc.get_dictionary_mut(font).unwrap().set("Widths",vec![Object::Integer(999)]);},
                    1 => {doc.get_dictionary_mut(id).unwrap().set("DA",Object::string_literal("/Helv 0 Tf 0 g"));},
                    2 => {doc.get_dictionary_mut(id).unwrap().set("MaxLen",-1);},
                    3 => {doc.get_dictionary_mut(id).unwrap().set("F",6);},
                    4 => {doc.get_dictionary_mut(form).unwrap().set("XFA",Object::string_literal("unsupported"));},
                    5 => {doc.get_dictionary_mut(form).unwrap().set("CO",vec![Object::Reference(id)]);},
                    6 => {doc.get_dictionary_mut(id).unwrap().set("ByteRange",vec![Object::Integer(0)]);},
                    7 => {let root=doc.trailer.get(b"Root").unwrap().as_reference().unwrap(); doc.get_dictionary_mut(root).unwrap().remove(b"AcroForm");},
                    8 => {doc.get_dictionary_mut(id).unwrap().set("AP",id);},
                    9 => {let mut extra=doc.get_dictionary(id).unwrap().clone(); extra.set("F",2); let extra=doc.add_object(extra); doc.get_dictionary_mut(page).unwrap().get_mut(b"Annots").unwrap().as_array_mut().unwrap().push(Object::Reference(extra));},
                    10 => {let ap=dict(doc,doc.get_dictionary(id).unwrap().get(b"AP").unwrap()).unwrap().get(b"N").unwrap().as_reference().unwrap(); let stream=doc.get_object_mut(ap).unwrap().as_stream_mut().unwrap(); stream.set_content(vec![b' ';MAX_AP+1]); stream.compress().unwrap();},
                    _ => {doc.get_dictionary_mut(id).unwrap().set("T",Object::string_literal(""));},
                }
            });
            let query=FormDocument::query(&source,1); assert_eq!(query.status,"unsupported","Trust-boundary case {kind}"); assert!(query.fields.is_empty());
        }
        let blank_without_ap=mutated(|doc| {let id=field_ids(doc)[1]; doc.get_dictionary_mut(id).unwrap().remove(b"AP");}); assert_eq!(FormDocument::query(&blank_without_ap,1).status,"supported");
    }
}
#[cfg(test)]
#[path = "forms_hardening_tests.rs"]
pub(crate) mod hardening;
#[derive(Clone, Debug)]
pub struct FormField { pub field_id: String, pub name: String, pub page: usize, pub value: String, pub max_length: Option<usize>, pub checked: Option<bool>, pub(crate) on_state: Option<String>, pub radio: Option<RadioInfo>, pub choice:Option<ChoiceInfo> }
#[derive(Clone,Debug)]
pub struct ChoiceInfo { pub presentation:&'static str, pub options:Vec<ChoiceOption>, pub selected_option_id:Option<String>, pub multiple:bool, pub editable:bool, pub selected_option_ids:Vec<String>, pub text:String }
#[derive(Clone,Debug,Serialize)]
#[serde(rename_all="camelCase")]
pub struct ChoiceOption { pub option_id:String, pub label:String, #[serde(skip)] pub(crate) export:String }
#[derive(Clone,Debug)]
pub struct RadioInfo { pub options:Vec<RadioOption>, pub selected_option_id:Option<String> }
#[derive(Clone,Debug,Serialize)]
#[serde(rename_all="camelCase")]
pub struct RadioOption { pub option_id:String, pub label:String, #[serde(skip)] pub(crate) object:ObjectId }
impl Serialize for FormField {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(if self.choice.is_some(){11}else{6}))?;
        map.serialize_entry("fieldId", &self.field_id)?; map.serialize_entry("name", &self.name)?; map.serialize_entry("page", &self.page)?;
        if let Some(choice)=&self.choice {map.serialize_entry("kind","choice")?;map.serialize_entry("presentation",choice.presentation)?;map.serialize_entry("options",&choice.options)?;map.serialize_entry("selectedOptionId",&choice.selected_option_id)?;map.serialize_entry("multiple",&choice.multiple)?;map.serialize_entry("editable",&choice.editable)?;map.serialize_entry("selectedOptionIds",&choice.selected_option_ids)?;map.serialize_entry("text",&choice.text)?;}
        else if let Some(radio)=&self.radio {map.serialize_entry("kind","radio")?;map.serialize_entry("options",&radio.options)?;map.serialize_entry("selectedOptionId",&radio.selected_option_id)?;}
        else if let Some(checked) = self.checked { map.serialize_entry("kind", "checkbox")?; map.serialize_entry("checked", &checked)?; }
        else { map.serialize_entry("kind", "text")?; map.serialize_entry("value", &self.value)?; map.serialize_entry("maxLength", &self.max_length)?; }
        map.end()
    }
}
#[derive(Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase", deny_unknown_fields)]
pub enum FieldValue {
    Text { #[serde(rename = "fieldId")] field_id: String, value: String },
    Checkbox { #[serde(rename = "fieldId")] field_id: String, checked: bool },
    Radio { #[serde(rename="fieldId")] field_id:String, #[serde(rename="optionId", deserialize_with="present_option")] option_id:Option<String> },
    Choice { #[serde(rename="fieldId")] field_id:String, #[serde(rename="optionId", deserialize_with="present_option")] option_id:Option<String> },
    Choices { #[serde(rename="fieldId")] field_id:String, #[serde(rename="optionIds")] option_ids:Vec<String> },
    Combo { #[serde(rename="fieldId")] field_id:String, text:String },
}
fn present_option<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> { Option::<String>::deserialize(deserializer) }
impl FieldValue { fn field_id(&self) -> &str { match self { Self::Text { field_id, .. } | Self::Checkbox { field_id, .. } | Self::Radio {field_id,..} | Self::Choice {field_id,..} | Self::Choices {field_id,..} | Self::Combo {field_id,..} => field_id } } }

#[derive(Clone)]
struct Style { width: f32, height: f32, border: f32, background: Option<Vec<f32>>, border_color: Option<Vec<f32>>, text_color: Vec<f32>, font: String, size: f32, font_object: Object, winansi: bool, top: usize }
struct ParsedField { dto: FormField, object: ObjectId, style: Option<Style>, on_state: Option<Vec<u8>> }
pub struct FormDocument { document: Document, fields: Vec<ParsedField>, pub pages: usize }

fn object<'a>(doc: &'a Document, value: &'a Object) -> Result<&'a Object, String> { doc.dereference(value).map(|(_, value)| value).map_err(|_| "The form contains an invalid or cyclic reference.".into()) }
fn dict<'a>(doc: &'a Document, value: &'a Object) -> Result<&'a Dictionary, String> { object(doc, value)?.as_dict().map_err(|_| "The form contains an invalid dictionary.".into()) }
fn keys(value: &Dictionary, allowed: &[&[u8]]) -> Result<(), String> { if value.iter().any(|(key, _)| !allowed.contains(&key.as_slice())) { Err("This form uses unsupported field, appearance, or layout properties.".into()) } else { Ok(()) } }
fn number(value: &Object) -> Result<f32, String> { let value = value.as_float().map_err(|_| "The form contains an invalid number.")?; if value.is_finite() { Ok(value) } else { Err("The form contains a non-finite number.".into()) } }
fn numbers(value: &Object) -> Result<Vec<f32>, String> { let values = value.as_array().map_err(|_| "The form contains an invalid numeric array.")?; if values.len() > 6 { return Err("The form numeric array is too large.".into()); } values.iter().map(number).collect() }
fn text(value: &Object) -> Result<String, String> { lopdf::decode_text_string(value).map_err(|_| "The form contains an unsupported text encoding.".into()) }
fn ascii(value: &str) -> Result<(), String> { if value.len() > MAX_VALUE { return Err("Each form value is limited to 4 KiB.".into()); } if !value.bytes().all(|byte| (0x20..=0x7e).contains(&byte)) { return Err("This form tool accepts printable ASCII text only, without tabs or newlines.".into()); } Ok(()) }
fn latin_char(character: char) -> bool { matches!(character as u32, 0x20..=0x7e | 0xa1..=0xff) && character != '\u{ad}' }
// Text values: printable ASCII plus the Latin-1 Supplement from U+00A1 (the same code points in WinAnsi; U+00A0 is refused because a font may remap code 160 to the euro sign), at most 4 KiB, no tabs or newlines.
fn latin(value: &str) -> Result<(), String> { if value.len() > MAX_VALUE { return Err("Each form value is limited to 4 KiB.".into()); } if !value.chars().all(latin_char) { return Err("This form tool accepts printable ASCII and Latin-1 characters only (no tabs, newlines, or other scripts).".into()); } Ok(()) }
fn win_bytes(value: &str) -> Vec<u8> { value.chars().map(|character| character as u32 as u8).collect() }
fn pdf_text(value: &str) -> Object { if value.is_ascii() { Object::string_literal(value.as_bytes().to_vec()) } else { let mut bytes = vec![0xfe, 0xff]; for unit in value.encode_utf16() { bytes.extend(unit.to_be_bytes()); } Object::string_literal(bytes) } }
// Helvetica advance widths (1/1000 em) for 0xA0..=0xFF, Adobe's Helvetica metrics for the WinAnsi Latin-1 Supplement.
const LATIN1_WIDTHS: [u16; 96] = [278,333,556,556,556,556,260,556,333,737,370,556,584,333,737,333, 400,584,333,333,333,556,537,278,333,333,365,556,834,834,834,611, 667,667,667,667,667,667,1000,722,667,667,667,667,278,278,278,278, 722,722,778,778,778,778,778,584,778,722,722,722,722,667,667,611, 556,556,556,556,556,556,889,500,556,556,556,556,278,278,278,278, 556,556,556,556,556,556,556,584,611,556,556,556,556,500,556,500];
fn advance(character: char) -> f32 { let code = character as u32; if code >= 0xa0 { LATIN1_WIDTHS[(code - 0xa0) as usize] as f32 } else { WIDTHS[(code - 32) as usize] as f32 } }
fn full_winansi(doc: &Document, value: &Object) -> bool {
    let Ok(font) = dict(doc, value) else { return false };
    let Some(encoding) = font.get(b"Encoding").ok().and_then(|value| object(doc, value).ok()) else { return false };
    if encoding.as_name().ok() == Some(b"WinAnsiEncoding") { return true; }
    let Ok(encoding) = encoding.as_dict() else { return false };
    let winansi_base = match encoding.get(b"BaseEncoding").and_then(Object::as_name).ok() { Some(b"WinAnsiEncoding") => true, None | Some(b"StandardEncoding") => false, _ => return false };
    let mut assigned = HashMap::new(); let mut code = 0i64;
    for value in encoding.get(b"Differences").and_then(Object::as_array).map(Vec::as_slice).unwrap_or(&[]) { if let Ok(index) = value.as_i64() { code = index; } else { if let Ok(name) = value.as_name() { assigned.insert(code, name.to_vec()); } let Some(next) = code.checked_add(1) else { return false }; code = next; } }
    // Every Latin-1 code we emit (0xA1..=0xFF except the soft hyphen) must show its Latin-1 glyph: assigned by name, or inherited from WinAnsi,
    // or inherited from StandardEncoding where that coincides.
    (161..=255i64).filter(|code| *code != 173).all(|code| match assigned.get(&code) { Some(name) => name.as_slice() == LATIN1_NAMES[(code - 161) as usize].as_bytes(), None => winansi_base || [161, 162, 163, 165, 167, 171, 182, 187, 191].contains(&code) })
}
const LATIN1_NAMES: [&str; 95] = ["exclamdown","cent","sterling","currency","yen","brokenbar","section","dieresis","copyright","ordfeminine","guillemotleft","logicalnot","hyphen","registered","macron","degree","plusminus","twosuperior","threesuperior","acute","mu","paragraph","periodcentered","cedilla","onesuperior","ordmasculine","guillemotright","onequarter","onehalf","threequarters","questiondown","Agrave","Aacute","Acircumflex","Atilde","Adieresis","Aring","AE","Ccedilla","Egrave","Eacute","Ecircumflex","Edieresis","Igrave","Iacute","Icircumflex","Idieresis","Eth","Ntilde","Ograve","Oacute","Ocircumflex","Otilde","Odieresis","multiply","Oslash","Ugrave","Uacute","Ucircumflex","Udieresis","Yacute","Thorn","germandbls","agrave","aacute","acircumflex","atilde","adieresis","aring","ae","ccedilla","egrave","eacute","ecircumflex","edieresis","igrave","iacute","icircumflex","idieresis","eth","ntilde","ograve","oacute","ocircumflex","otilde","odieresis","divide","oslash","ugrave","uacute","ucircumflex","udieresis","yacute","thorn","ydieresis"];
fn color(value: &Object) -> Result<Vec<f32>, String> { let value = numbers(value)?; if !matches!(value.len(), 1 | 3) || value.iter().any(|x| !(0.0..=1.0).contains(x)) { return Err("Only gray or RGB form colors are supported.".into()); } Ok(value) }
fn real(values: &[f32]) -> Vec<Object> { values.iter().copied().map(Object::Real).collect() }
fn op(name: &str, values: &[f32]) -> Operation { Operation::new(name, real(values)) }
fn paint(values: &[f32], stroke: bool) -> Operation { op(match (values.len(), stroke) { (1, false) => "g", (1, true) => "G", (_, false) => "rg", (_, true) => "RG" }, values) }

fn appearance(style: &Style, value: &str) -> Content<Vec<Operation>> {
    let mut operations = Vec::new();
    if let Some(background) = &style.background { operations.extend([paint(background, false), op("re", &[0.0, 0.0, style.width, style.height]), op("f", &[])]); }
    if let Some(border) = &style.border_color { if style.border > 0.0 { operations.extend([paint(border, true), op("w", &[style.border]), op("re", &[style.border / 2.0, style.border / 2.0, style.width - style.border, style.height - style.border]), op("s", &[])]); } }
    let inset = style.border * 2.0;
    operations.extend([Operation::new("BMC", vec![Object::Name(b"Tx".to_vec())]), op("q", &[]), op("re", &[inset, inset, style.width - 2.0 * inset, style.height - 2.0 * inset]), op("W", &[]), op("n", &[]), op("g", &[0.0]), op("G", &[0.0])]);
    if !value.is_empty() { operations.extend([op("BT", &[]), Operation::new("Tf", vec![Object::Name(style.font.as_bytes().to_vec()), Object::Real(style.size)]), paint(&style.text_color, false), op("Tm", &[1.0, 0.0, 0.0, 1.0, style.border * 4.0, style.height - style.size - inset]), Operation::new("Tj", vec![Object::string_literal(win_bytes(value))]), op("ET", &[])]); }
    operations.extend([op("Q", &[]), op("EMC", &[])]);
    Content { operations }
}
// A filled copy keeps only objects reachable from the trailer, so replaced appearances and superseded revisions do not carry old values.
use crate::pdf_load::reachable;
fn without_object_streams(id: ObjectId, object: &mut Object) -> Option<(ObjectId, Object)> {
    if matches!(object, Object::Stream(stream) if stream.dict.has_type(b"ObjStm")) { None } else { Some((id, std::mem::replace(object, Object::Null))) }
}
fn resolves_to_zero(document: &Document, stream: &Stream) -> bool {
    stream.dict.get(b"Length").and_then(|value| document.dereference(value)).ok().is_some_and(|(_, value)| match value { Object::Integer(0) => true, Object::Real(real) => *real == 0.0, _ => false })
}
// Refuses a load whose copy would silently differ from the source: an in-use object that failed to load,
// a stale object-stream copy standing in for a newest copy that failed to parse, or a stream left empty by an unresolvable /Length.
pub(crate) fn check_faithful_load(document: &Document, source: &[u8]) -> Result<(), String> {
    const DROPPED: &str = "Some PDF objects could not be loaded within the form-loading limits; a filled copy would drop them.";
    for object in document.objects.values() { if let Object::Stream(stream) = object { if stream.content.is_empty() && !resolves_to_zero(document, stream) { return Err("A PDF stream could not be read (its /Length does not resolve); a filled copy would drop its content.".into()); } } }
    // An indirect /Length that resolved late (to 0) leaves a stream empty although data follows: the raw bytes must go straight to `endstream`.
    let header = source.windows(5).position(|window| window == b"%PDF-").unwrap_or(0);
    for object in document.objects.values() { if let Object::Stream(stream) = object { if let (true, Some(start)) = (stream.content.is_empty(), stream.start_position) {
        let tail = source.get(header + start..).unwrap_or(&[]); let tail = tail.strip_prefix(b"\r\n".as_slice()).or_else(|| tail.strip_prefix(b"\n")).or_else(|| tail.strip_prefix(b"\r")).unwrap_or(tail);
        if !tail.starts_with(b"endstream") { return Err("A PDF stream's /Length resolves to zero but data follows; a filled copy would drop its content.".into()); }
    } } }
    // Every in-use entry must have loaded. An `n` entry at offset 0 is the unused `0000000000 00000 n` row some writers emit: offset 0
    // is the file header, so it cannot hold an object and viewers skip it.
    if document.reference_table.entries.iter().any(|(number, entry)| match entry { XrefEntry::Normal { offset, generation } => *offset != 0 && !document.objects.contains_key(&(*number, *generation)), XrefEntry::Compressed { .. } => !document.objects.contains_key(&(*number, 0)), _ => false }) { return Err(DROPPED.into()); }
    // lopdf substitutes an older object-stream copy of a number when its newest (direct) copy fails to parse. Reload without the
    // object streams: a direct entry must then come from its own offset, never from the stale copy.
    if document.objects.values().any(|object| matches!(object, Object::Stream(stream) if stream.dict.has_type(b"ObjStm"))) {
        let direct: Vec<ObjectId> = document.reference_table.entries.iter().filter_map(|(number, entry)| match entry { XrefEntry::Normal { offset, generation } if *offset != 0 => Some((*number, *generation)), _ => None }).filter(|id| !matches!(document.objects.get(id), Some(Object::Stream(stream)) if stream.dict.has_type(b"ObjStm"))).collect();
        if !direct.is_empty() {
            let mut options = LoadOptions::with_filter(without_object_streams); options.max_decompressed_size = Some(MAX_TEXT);
            let Ok(direct_only) = Document::load_mem_with_options(source, options) else { return Err(DROPPED.into()) };
            if direct.iter().any(|id| !direct_only.objects.contains_key(id)) { return Err("A PDF object's newest copy could not be read and an older copy was substituted; a filled copy would change it.".into()); }
        }
    }
    Ok(())
}
fn same_object(a: &Object, b: &Object) -> bool {
    match (a, b) { (Object::Integer(_) | Object::Real(_), Object::Integer(_) | Object::Real(_)) => number(a).ok().zip(number(b).ok()).is_some_and(|(a, b)| (a - b).abs() < 0.0001), _ => a == b }
}
fn same_appearance(actual: &Content<Vec<Operation>>, expected: &Content<Vec<Operation>>) -> bool {
    actual.operations.len() == expected.operations.len() && actual.operations.iter().zip(&expected.operations).all(|(a, b)| a.operator == b.operator && a.operands.len() == b.operands.len() && a.operands.iter().zip(&b.operands).all(|(a, b)| same_object(a, b)))
}

// Checkbox artwork is retained verbatim: this validator never generates a glyph or substitutes a style.
fn checkbox(doc: &Document, field: &Dictionary, ap_bytes: &mut usize) -> Result<(Vec<u8>, bool), String> { button_appearance(doc,field,ap_bytes,false) }
fn button_appearance(doc: &Document, field: &Dictionary, ap_bytes: &mut usize, radio:bool) -> Result<(Vec<u8>, bool), String> {
    let rect = numbers(field.get(b"Rect").map_err(|_| "The checkbox rectangle is missing.")?)?;
    if rect.len() != 4 { return Err("The checkbox rectangle is invalid.".into()); }
    let (width, height) = (rect[2]-rect[0], rect[3]-rect[1]);
    if !(8.0..=256.0).contains(&width) || (width-height).abs() > 0.0001 { return Err("Only bounded square checkbox widgets are supported.".into()); }
    if field.get(b"H").ok().is_some_and(|value| value.as_name().ok() != Some(b"N")) { return Err("The checkbox interaction appearance is unsupported.".into()); }
    if let Ok(bs) = field.get(b"BS") { let bs = dict(doc, bs)?; keys(bs, &[b"S",b"W"])?; if bs.get(b"S").and_then(Object::as_name).ok() != Some(b"S") || !(0.0..=4.0).contains(&number(bs.get(b"W").map_err(|_| "Missing checkbox border width.")?)?) { return Err("Only bounded solid checkbox borders are supported.".into()); } }
    if let Ok(mk) = field.get(b"MK") { let mk = dict(doc,mk)?; keys(mk,&[b"CA",b"BC",b"BG"])?; for key in [b"BC".as_slice(),b"BG"] { if let Ok(value)=mk.get(key) { color(value)?; } } if let Ok(value)=mk.get(b"CA") { let caption=text(value)?; if caption.len()>8 || !caption.bytes().all(|byte| (32..=126).contains(&byte)) { return Err("The checkbox caption is unsupported.".into()); } } }
    let ap = dict(doc, field.get(b"AP").map_err(|_| "The checkbox appearance is missing.")?)?; keys(ap, &[b"N",b"R",b"D"])?;
    if !ap.has(b"N") { return Err("The checkbox normal appearance is missing.".into()); }
    let mut on_state: Option<Vec<u8>> = None;
    for (_, value) in ap {
        let states = dict(doc,value)?;
        if states.len()!=2 || !states.has(b"Off") { return Err("A checkbox requires Off and one distinct on appearance state.".into()); }
        let on = states.iter().find(|(name,_)| name.as_slice()!=b"Off").map(|(name,_)|name).ok_or("The checkbox on state is missing.")?;
        if on.is_empty() || on.len()>64 || if radio {!on.iter().all(|byte|(32..=126).contains(byte)) || on.iter().all(|byte|*byte==b' ')}else{!on.iter().all(u8::is_ascii_alphanumeric)} { return Err("The button on-state name is unsupported.".into()); }
        if on_state.as_ref().is_some_and(|state|state!=on) { return Err("The checkbox appearance state names disagree.".into()); }
        on_state=Some(on.clone());
        let mut decoded_states = Vec::new();
        for (_, value) in states {
            let stream=object(doc,value)?.as_stream().map_err(|_| "The checkbox state appearance is not a stream.")?;
            keys(&stream.dict,&[b"Type",b"Subtype",b"FormType",b"BBox",b"Matrix",b"Resources",b"Length",b"Filter"])?;
            if stream.dict.get(b"Subtype").and_then(Object::as_name).ok()!=Some(b"Form") || stream.dict.get(b"Type").ok().is_some_and(|value|value.as_name().ok()!=Some(b"XObject")) || stream.dict.get(b"FormType").ok().is_some_and(|value|value.as_i64().ok()!=Some(1)) { return Err("The checkbox appearance type is unsupported.".into()); }
            let bbox=numbers(stream.dict.get(b"BBox").map_err(|_| "Missing checkbox appearance bounds.")?)?;
            if bbox.len()!=4 || !bbox.iter().zip([0.0,0.0,width,height]).all(|(a,b)|(a-b).abs()<0.0001) { return Err("The checkbox appearance bounds disagree with its widget.".into()); }
            if stream.dict.get(b"Matrix").ok().map(numbers).transpose()?.is_some_and(|matrix|matrix!=vec![1.0,0.0,0.0,1.0,0.0,0.0]) { return Err("Transformed checkbox appearances are unsupported.".into()); }
            if let Ok(resources)=stream.dict.get(b"Resources") { let resources=dict(doc,resources)?; keys(resources,&[b"ProcSet"])?; if resources.get(b"ProcSet").ok().is_some_and(|value|value.as_array().ok().is_none_or(|values|values.as_slice()!=[Object::Name(b"PDF".to_vec())])) { return Err("The checkbox appearance resources are unsupported.".into()); } }
            let decoded=stream.decompressed_content_with_limit(MAX_AP).map_err(|_| "The checkbox appearance exceeds its decoding limit or uses an unsupported filter.")?;
            *ap_bytes+=decoded.len(); if *ap_bytes>MAX_TEXT { return Err("The form appearances exceed 1 MiB.".into()); }
            let content=Content::decode(&decoded).map_err(|_| "The checkbox appearance cannot be parsed.")?;
            if content.operations.is_empty() || content.operations.len()>256 { return Err("The checkbox appearance exceeds its operator limit.".into()); }
            let mut stack=Vec::new();let mut offset=(0.0f32,0.0f32); let mut path=false; let mut painted=0;
            for operation in &content.operations {
                let count=match operation.operator.as_str() { "q"|"Q"|"h"|"f"|"f*"|"s"|"S"|"n"=>0,"g"|"G"|"w"=>1,"m"|"l"=>2,"rg"|"RG"=>3,"re"=>4,"c"=>6,"cm" if radio=>6,_=>return Err("The button uses unsupported appearance operators.".into()) };
                if operation.operands.len()!=count { return Err("The checkbox appearance operator is malformed.".into()); }
                let values=operation.operands.iter().map(number).collect::<Result<Vec<_>,_>>()?;
                if values.iter().any(|value|value.abs()>10000.0) { return Err("The checkbox appearance number exceeds its limit.".into()); }
                match operation.operator.as_str() {
                    "q"=>{if path || stack.len()>=8 { return Err("The button graphics stack is unsupported.".into()); } stack.push(offset);},
                    "Q"=>{if path { return Err("The button graphics stack is unbalanced.".into()); } offset=stack.pop().ok_or("The button graphics stack is unbalanced.")?;},
                    "cm"=>{if path || stack.is_empty() || offset!=(0.0,0.0) || values!=vec![1.0,0.0,0.0,1.0,width/2.0,height/2.0] {return Err("Only one safe centered radio appearance translation is supported per graphics state.".into());}offset=(width/2.0,height/2.0);},
                    "g"|"G"|"rg"|"RG"=>{color(&Object::Array(operation.operands.clone()))?;},
                    "w"=>{if !(0.0..=4.0).contains(&values[0]) {return Err("The checkbox stroke width is unsupported.".into());}},
                    "m"|"l"|"c"=>{if operation.operator!="m" && !path { return Err("The button path is malformed.".into()); } if values.chunks_exact(2).any(|point|!(0.0..=width).contains(&(point[0]+offset.0)) || !(0.0..=height).contains(&(point[1]+offset.1))) { return Err("The button path is outside its appearance bounds.".into()); } path=true;},
                    "re"=>{if values[0]+offset.0<0.0 || values[1]+offset.1<0.0 || values[2]<=0.0 || values[3]<=0.0 || values[0]+offset.0+values[2]>width || values[1]+offset.1+values[3]>height { return Err("The button rectangle path is out of bounds.".into()); } path=true;},
                    "h"=>{if !path {return Err("The checkbox path is malformed.".into());}},
                    "f"|"f*"|"s"|"S"=>{if !path {return Err("The checkbox paint has no path.".into());} painted+=1;path=false;},
                    "n"=>{path=false;},_=>unreachable!(),
                }
            }
            if !stack.is_empty() || path || painted==0 { return Err("The button appearance is incomplete or unbalanced.".into()); }
            decoded_states.push(content);
        }
        if same_appearance(&decoded_states[0],&decoded_states[1]) { return Err("The checkbox on and Off appearances must differ.".into()); }
    }
    let on=on_state.ok_or("The checkbox on state is missing.")?;
    let value=field.get(b"V").and_then(Object::as_name).map_err(|_| "The checkbox value state is missing or invalid.")?;
    if value!=b"Off" && value!=on || field.get(b"AS").and_then(Object::as_name).ok()!=Some(value) {return Err("The checkbox value and appearance states disagree.".into());}
    if field.get(b"DV").ok().is_some_and(|value|value.as_name().ok().is_none_or(|value|value!=b"Off" && value!=on)) { return Err("The checkbox default state is unsupported.".into()); }
    let checked=value==on; Ok((on,checked))
}

fn font(doc: &Document, value: &Object) -> Result<(), String> {
    let font = dict(doc, value)?; keys(font, &[b"Type", b"Subtype", b"BaseFont", b"Name", b"Encoding"])?;
    if font.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Type1") || font.get(b"BaseFont").and_then(Object::as_name).ok() != Some(b"Helvetica") { return Err("Only standard Helvetica form fonts are supported.".into()); }
    let encoding = object(doc, font.get(b"Encoding").map_err(|_| "The form font encoding is missing.")?)?;
    if encoding.as_name().ok() == Some(b"WinAnsiEncoding") { return Ok(()); }
    let encoding = encoding.as_dict().map_err(|_| "The form font encoding is unsupported.")?; keys(encoding, &[b"Type", b"BaseEncoding", b"Differences"])?;
    let base = encoding.get(b"BaseEncoding").and_then(Object::as_name).ok();
    if base.is_some() && base != Some(b"WinAnsiEncoding") && base != Some(b"StandardEncoding") { return Err("The form font encoding is unsupported.".into()); }
    let differences = encoding.get(b"Differences").and_then(Object::as_array).map_err(|_| "The form font differences are unsupported.")?;
    if differences.len() > 512 { return Err("The form font encoding is too large.".into()); }
    let mut code = 0; let mut quote = base == Some(b"WinAnsiEncoding"); let mut grave = quote;
    for value in differences {
        if let Ok(index) = value.as_i64() { if !(0..=255).contains(&index) { return Err("Invalid form font encoding index.".into()); } code = index; }
        else { let name = value.as_name().map_err(|_| "Invalid form font glyph.")?; if code > 255 { return Err("Invalid form font encoding range.".into()); } if (32..=126).contains(&code) { if code == 39 && name == b"quotesingle" { quote = true; } else if code == 96 && name == b"grave" { grave = true; } else { return Err("The form remaps printable ASCII glyphs.".into()); } } code += 1; }
    }
    if !quote || !grave { return Err("The form does not use the supported ASCII quote encoding.".into()); } Ok(())
}

fn radio_group(doc:&Document,parent:&Dictionary,id:ObjectId,widgets:&HashMap<ObjectId,usize>,pages:&[ObjectId],consumed:&mut HashSet<ObjectId>,ap_bytes:&mut usize) -> Result<(usize,RadioInfo,String),String> {
    keys(parent,&[b"FT",b"Ff",b"T",b"TU",b"Kids",b"V",b"DV"])?;
    if parent.get(b"FT").and_then(Object::as_name).ok()!=Some(b"Btn") || !matches!(parent.get(b"Ff").and_then(Object::as_i64).ok(),Some(49152|49154)) || widgets.contains_key(&id) {return Err("Only one-level Radio and NoToggleToOff groups with optional Required are supported.".into());}
    let kids=parent.get(b"Kids").and_then(Object::as_array).map_err(|_|"The radio child list is invalid.")?;
    if !(2..=32).contains(&kids.len()){return Err("Each radio group requires between 2 and 32 options.".into());}
    let value=parent.get(b"V").ok().map(|value|value.as_name().map_err(|_|"The radio current value is invalid.")).transpose()?;
    let mut options=Vec::new();let mut states=HashSet::new();let mut group_page=None;let mut selected=None;
    for kid in kids {
        let widget_id=kid.as_reference().map_err(|_|"The radio widget must be a reference.")?;
        if !consumed.insert(widget_id){return Err("A radio widget is repeated or shared between fields.".into());}
        let page=*widgets.get(&widget_id).ok_or("A radio widget has no canonical page annotation.")?;
        if group_page.is_some_and(|owner|owner!=page){return Err("Radio groups spanning multiple pages are unsupported.".into());}group_page=Some(page);
        let widget=doc.get_dictionary(widget_id).map_err(|_|"The radio widget is invalid.")?;
        keys(widget,&[b"Type",b"Subtype",b"FT",b"Parent",b"P",b"F",b"Rect",b"AP",b"AS",b"BS",b"MK",b"H"])?;
        if widget.get(b"Type").ok().is_some_and(|value|value.as_name().ok()!=Some(b"Annot")) || widget.get(b"Subtype").and_then(Object::as_name).ok()!=Some(b"Widget") || widget.get(b"FT").ok().is_some_and(|value|value.as_name().ok()!=Some(b"Btn")) || widget.get(b"Parent").and_then(Object::as_reference).ok()!=Some(id) || widget.get(b"P").and_then(Object::as_reference).ok()!=Some(pages[page]) || widget.get(b"F").and_then(Object::as_i64).ok()!=Some(4) {return Err("The radio widget flags, parent, or page relationship is unsupported.".into());}
        let mut appearance_widget=widget.clone();appearance_widget.set("V",widget.get(b"AS").map_err(|_|"The radio appearance state is missing.")?.clone());
        let(on,is_selected)=button_appearance(doc,&appearance_widget,ap_bytes,true)?;
        if !states.insert(on.clone()){return Err("Radio option states must be distinct, without radios in unison.".into());}
        let label=String::from_utf8(on).map_err(|_|"The radio source option name is unsupported.")?;let option_id=format!("option-{}-{}",widget_id.0,widget_id.1);
        if is_selected {if selected.is_some(){return Err("Multiple selected radio widgets are unsupported.".into());}if value!=Some(label.as_bytes()){return Err("The selected radio widget and parent value disagree.".into());}selected=Some(option_id.clone());}
        options.push(RadioOption {option_id,label,object:widget_id});
    }
    if selected.is_none() && value.is_some_and(|value|value!=b"Off"){return Err("The blank radio group and parent value disagree.".into());}
    if parent.get(b"DV").ok().is_some_and(|value|value.as_name().ok().is_none_or(|value|value!=b"Off"&&!states.contains(value))){return Err("The radio default must be Off or a valid source option state.".into());}
    let current=selected.as_ref().and_then(|selected|options.iter().find(|option|&option.option_id==selected)).map(|option|option.label.clone()).unwrap_or_default();
    Ok((group_page.ok_or("The radio page is missing.")?,RadioInfo {options,selected_option_id:selected},current))
}

fn validate_text_appearance(document:&Document,field:&Dictionary,style:&Style,expected:&Content<Vec<Operation>>,required:bool,ap_bytes:&mut usize)->Result<(),String> {
if let Ok(ap) = field.get(b"AP") {
    let ap = dict(&document, ap)?; keys(ap, &[b"N"])?;
    let stream = object(&document, ap.get(b"N").map_err(|_| "The form normal appearance is missing.")?)?.as_stream().map_err(|_| "The form normal appearance is invalid.")?;
    keys(&stream.dict, &[b"Type", b"Subtype", b"FormType", b"BBox", b"Matrix", b"Resources", b"Length", b"Filter"])?;
    if stream.dict.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Form") || stream.dict.get(b"FormType").ok().is_some_and(|value| value.as_i64().ok() != Some(1)) { return Err("The form appearance type is unsupported.".into()); }
    if stream.dict.get(b"Type").ok().is_some_and(|value| value.as_name().ok() != Some(b"XObject")) { return Err("The form appearance object type is invalid.".into()); }
    let bbox = numbers(stream.dict.get(b"BBox").map_err(|_| "The form appearance bounds are missing.")?)?;
    if bbox.len() != 4 || !bbox.iter().zip([0.0, 0.0, style.width, style.height]).all(|(a,b)| (a-b).abs() < 0.0001) { return Err("The form appearance bounds do not match its widget.".into()); }
    if stream.dict.get(b"Matrix").ok().map(numbers).transpose()?.is_some_and(|matrix| matrix != vec![1.0,0.0,0.0,1.0,0.0,0.0]) { return Err("Rotated or transformed form appearances are unsupported.".into()); }
    let resources = dict(&document, stream.dict.get(b"Resources").map_err(|_| "The form appearance resources are missing.")?)?; keys(resources, &[b"Font", b"ProcSet"])?;
    if resources.get(b"ProcSet").ok().is_some_and(|value| value.as_array().ok().is_none_or(|values| values.as_slice() != [Object::Name(b"PDF".to_vec()), Object::Name(b"Text".to_vec())])) { return Err("The form appearance procedure resources are unsupported.".into()); }
    let fonts = dict(&document, resources.get(b"Font").map_err(|_| "The appearance font resource is missing.")?)?;
    if fonts.len() != 1 { return Err("The appearance font resources are unsupported.".into()); } font(&document, fonts.get(style.font.as_bytes()).map_err(|_| "The appearance font differs from the form font.")?)?;
    let decoded = stream.decompressed_content_with_limit(MAX_AP).map_err(|_| "The form appearance exceeds its decoding limit or uses an unsupported filter.")?; *ap_bytes += decoded.len(); if *ap_bytes > MAX_TEXT { return Err("The form appearances exceed 1 MiB.".into()); }
    if !same_appearance(&Content::decode(&decoded).map_err(|_| "The form appearance cannot be parsed.")?, expected) { return Err("The form contains an unknown appearance or layout; filling it could discard artwork.".into()); }
} else if required { return Err("A populated form field without its original appearance is unsupported.".into()); }
    Ok(())
}

fn style(doc: &Document, form: &Dictionary, field: &Dictionary) -> Result<Style, String> {
    let rect = numbers(field.get(b"Rect").map_err(|_| "A form field has no rectangle.")?)?;
    if rect.len() != 4 { return Err("A form field rectangle is malformed.".into()); }
    let (width, height) = (rect[2] - rect[0], rect[3] - rect[1]);
    if width < 8.0 || height < 8.0 || width > 10000.0 || height > 10000.0 { return Err("The form field dimensions are unsupported.".into()); }
    let da = field.get(b"DA").or_else(|_| form.get(b"DA")).map_err(|_| "The form has no default text appearance.")?.as_str().map_err(|_| "The form text appearance is invalid.")?;
    if da.len() > 1024 { return Err("The form text appearance is too large.".into()); }
    let da = Content::decode(da).map_err(|_| "The form text appearance cannot be parsed.")?;
    if da.operations.len() != 2 || da.operations[0].operator != "Tf" || da.operations[0].operands.len() != 2 || !matches!(da.operations[1].operator.as_str(), "g" | "rg") { return Err("The form uses unsupported text appearance operators.".into()); }
    let name = da.operations[0].operands[0].as_name().map_err(|_| "The form font name is invalid.")?;
    if name.is_empty() || name.len() > 64 || !name.iter().all(u8::is_ascii_alphanumeric) { return Err("The form font name is unsupported.".into()); }
    let size = number(&da.operations[0].operands[1])?;
    if !(4.0..=72.0).contains(&size) { return Err("Automatic or unsupported form font sizes are unavailable.".into()); }
    let text_color = color(&Object::Array(da.operations[1].operands.clone()))?;
    if da.operations[1].operator == "g" && text_color.len() != 1 || da.operations[1].operator == "rg" && text_color.len() != 3 { return Err("The form text color is malformed.".into()); }
    let resources = dict(doc, form.get(b"DR").map_err(|_| "The form font resources are missing.")?)?; keys(resources, &[b"Font", b"Encoding"])?;
    let fonts = dict(doc, resources.get(b"Font").map_err(|_| "The form fonts are missing.")?)?;
    if fonts.len() > 16 { return Err("The form font resource list is too large.".into()); }
    let font_object = fonts.get(name).map_err(|_| "The form font resource cannot be found.")?.clone(); font(doc, &font_object)?;
    let mut border = 0.0;
    if let Ok(value) = field.get(b"BS") { let value = dict(doc, value)?; keys(value, &[b"S", b"W"])?; if value.get(b"S").and_then(Object::as_name).ok() != Some(b"S") { return Err("Only solid form borders are supported.".into()); } border = number(value.get(b"W").map_err(|_| "The form border width is missing.")?)?; }
    if !(0.0..=4.0).contains(&border) { return Err("The form border width is unsupported.".into()); }
    let (background, border_color) = if let Ok(value) = field.get(b"MK") { let value = dict(doc, value)?; keys(value, &[b"BG", b"BC"])?; (value.get(b"BG").ok().map(color).transpose()?, value.get(b"BC").ok().map(color).transpose()?) } else { (None, None) };
    if border > 0.0 && border_color.is_none() { return Err("The form border color is missing.".into()); }
    if width <= 8.0 * border || height < 1.225 * size + 4.0 * border { return Err("The form layout cannot fit its fixed text size without clipping descenders.".into()); }
    Ok(Style { width, height, border, background, border_color, text_color, font: String::from_utf8(name.to_vec()).map_err(|_| "Invalid form font name.")?, size, winansi: full_winansi(doc, &font_object), top: 0, font_object })
}

// Standard Helvetica ASCII advance widths, matching ReportLab's standard-font metrics (1/1000 em).
const WIDTHS: [u16; 95] = [278,278,355,556,556,889,667,191,333,333,389,584,278,333,278,278,556,556,556,556,556,556,556,556,556,556,278,278,584,584,584,556,1015,667,667,722,722,667,611,778,722,278,500,667,556,833,722,778,667,778,722,667,611,722,667,944,667,667,611,278,278,278,469,556,333,556,556,500,556,556,278,556,556,222,222,500,222,833,556,556,556,556,333,500,278,556,500,722,500,500,500,334,260,334,584];
fn fits(field: &ParsedField, value: &str) -> Result<(), String> {
    if field.dto.choice.is_some(){return Err("The submitted form patch has the wrong field kind.".into());}
    let style = field.style.as_ref().ok_or("The submitted form patch has the wrong field kind.")?;
    fits_style(style,value,field.dto.max_length)
}
fn fits_style(style:&Style,value:&str,max_length:Option<usize>)->Result<(),String> {
    latin(value)?;
    if !value.is_ascii() && !style.winansi { return Err("This field's font encoding cannot show characters beyond ASCII.".into()); }
    if max_length.is_some_and(|max| value.chars().count() > max) { return Err("The form value exceeds the field's MaxLength.".into()); }
    let advance = value.chars().map(advance).sum::<f32>() * style.size / 1000.0;
    if advance > style.width - 8.0 * style.border - 1.0 || advance + 0.04 * style.size > style.width - 6.0 * style.border || value.starts_with(['j','/']) && style.border * 2.0 < 0.03 * style.size { return Err("The text does not fit this field at its existing font size without clipping. Use a shorter value.".into()); } Ok(())
}

fn list_slots(style:&Style)->usize{((style.height-4.0*style.border)/(1.2*style.size)).floor() as usize}
fn selected_indices(choice:&ChoiceInfo)->Vec<usize>{if choice.multiple{choice.options.iter().enumerate().filter(|(_,option)|choice.selected_option_ids.contains(&option.option_id)).map(|(index,_)|index).collect()}else{choice.options.iter().position(|option|Some(&option.option_id)==choice.selected_option_id.as_ref()).into_iter().collect()}}
fn choice_appearance(style:&Style,choice:&ChoiceInfo)->Result<Content<Vec<Operation>>,String> {
    let selected=selected_indices(choice);
    if choice.presentation=="dropdown"{return Ok(appearance(style,selected.first().map(|index|choice.options[*index].label.as_str()).unwrap_or(choice.text.as_str())));}
    let inner=style.height-4.0*style.border;let slots=list_slots(style);
    if slots==0{return Err("This list box is too short to display any option.".into());}
    let top=style.top;if top>0&&top+slots>choice.options.len().max(slots){return Err("The list box scroll position is out of range.".into());}
    let visible=&choice.options[top.min(choice.options.len())..(top+slots).min(choice.options.len())];
    let leading=inner/slots as f32;let last=style.height-2.0*style.border-style.size-(visible.len()-1) as f32*leading;
    if last-0.225*style.size<2.0*style.border{return Err("The list box cannot display its options without clipping descenders.".into());}
    let mut content=appearance(style,"");content.operations.truncate(content.operations.len()-4);
    for index in selected.iter().copied().filter(|index|(top..top+slots).contains(index)){content.operations.extend([op("rg",&[0.600006,0.756866,0.854904]),op("re",&[2.0*style.border,style.height-2.0*style.border-(index-top+1) as f32*leading,style.width-4.0*style.border,leading]),op("f",&[])]);}
    content.operations.extend([op("g",&[0.0]),op("G",&[0.0])]);
    for(row,option)in visible.iter().enumerate(){let index=top+row;content.operations.push(op("BT",&[]));if row==0{content.operations.push(Operation::new("Tf",vec![Object::Name(style.font.as_bytes().to_vec()),Object::Real(style.size)]));}content.operations.extend([if selected.contains(&index){op("g",&[0.0])}else{paint(&style.text_color,false)},op("Td",&[4.0*style.border,style.height-2.0*style.border-style.size-row as f32*leading]),Operation::new("Tj",vec![Object::string_literal(win_bytes(&option.label))]),op("ET",&[])]);}
    content.operations.extend([op("Q",&[]),op("EMC",&[])]);Ok(content)
}
fn choice_field(document:&Document,form:&Dictionary,field:&Dictionary,id:ObjectId,ap_bytes:&mut usize)->Result<(ChoiceInfo,Style,String),String> {
    let flags=field.get(b"Ff").ok().map(|value|value.as_i64().map_err(|_|"The choice field flags are invalid.")).transpose()?.unwrap_or(0);
    let(presentation,multiple,editable)=match flags&!2{0=>("list",false,false),2097152=>("list",true,false),131072=>("dropdown",false,false),393216=>("dropdown",false,true),_=>return Err("Only dropdowns, editable combo boxes, and single or multi-select list boxes without sorting or commit-on-change flags are supported.".into())};
    let top_index=match field.get(b"TI").ok(){None=>0,Some(value)=>usize::try_from(value.as_i64().map_err(|_|"The choice top index is invalid.")?).map_err(|_|"The choice top index is invalid.")?};
    if presentation=="dropdown"&&top_index!=0{return Err("Dropdowns with a scrolled top index are unsupported.".into());}
    let opts=object(document,field.get(b"Opt").map_err(|_|"The choice options are missing.")?)?.as_array().map_err(|_|"The choice options are invalid.")?;
    if !(1..=32).contains(&opts.len()){return Err("Each choice field requires between one and 32 options.".into());}
    let mut options=Vec::new();let mut exports=HashSet::new();let mut labels=HashSet::new();
    for(index,value)in opts.iter().enumerate(){let value=object(document,value)?;let(export,label)=match value{Object::String(..)=>{let value=text(value)?;(value.clone(),value)},Object::Array(pair) if pair.len()==2=>(text(object(document,&pair[0])?)?,text(object(document,&pair[1])?)?),_=>return Err("A choice option must be a string or an export/display string pair.".into())};ascii(&export)?;ascii(&label)?;if export.trim().is_empty()||label.trim().is_empty()||!exports.insert(export.clone())||!labels.insert(label.clone()){return Err("Choice export values and display labels must be nonblank and distinct.".into());}options.push(ChoiceOption {option_id:format!("choice-{}-{}-{index}",id.0,id.1),label,export});}
    let values:Vec<String>=match field.get(b"V").ok().map(|value|object(document,value)).transpose()?{None=>Vec::new(),Some(Object::Array(items)) if multiple=>items.iter().map(|item|text(object(document,item)?)).collect::<Result<Vec<_>,_>>()?,Some(value)=>{let value=text(value)?;if value.is_empty(){Vec::new()}else{vec![value]}}};
    if values.len()>options.len(){return Err("The choice has more selected values than options.".into());}
    let mut selected_indices:Vec<usize>=Vec::new();let mut custom=String::new();
    for value in &values{if editable{latin(value)?;}else{ascii(value)?;}match options.iter().position(|option|option.export==*value){Some(index)=>{if selected_indices.contains(&index){return Err("The choice selects an option twice.".into());}selected_indices.push(index);},None if editable&&values.len()==1=>custom=value.clone(),None=>return Err("The choice value does not match a source export option.".into())}}
    selected_indices.sort_unstable();
    if let Ok(indices)=field.get(b"I"){let indices=object(document,indices)?.as_array().map_err(|_|"The choice selected indices are invalid.")?;if indices.len()!=selected_indices.len()||indices.iter().zip(&selected_indices).any(|(actual,wanted)|actual.as_i64().ok()!=Some(*wanted as i64)){return Err("The choice value and selected index disagree.".into());}}
    if let Ok(default)=field.get(b"DV"){let default=text(default)?;if editable{latin(&default)?;}else{ascii(&default)?;if !default.is_empty()&&!options.iter().any(|option|option.export==default){return Err("The choice default does not match a source export option.".into());}}}
    let value=if multiple{selected_indices.iter().map(|index|options[*index].export.as_str()).collect::<Vec<_>>().join("\n")}else if !custom.is_empty(){custom.clone()}else{values.first().cloned().unwrap_or_default()};
    let selected_ids:Vec<String>=selected_indices.iter().map(|index|options[*index].option_id.clone()).collect();
    let choice=ChoiceInfo {presentation,selected_option_id:if multiple{None}else{selected_ids.first().cloned()},multiple,editable,selected_option_ids:if multiple{selected_ids}else{Vec::new()},text:custom.clone(),options};let mut style=style(document,form,field)?;style.top=top_index;for option in &choice.options{fits_style(&style,&option.label,None)?;}if !custom.is_empty(){fits_style(&style,&custom,None)?;}let expected=choice_appearance(&style,&choice)?;validate_text_appearance(document,field,&style,&expected,presentation=="list"||!selected_indices.is_empty()||!custom.is_empty(),ap_bytes)?;Ok((choice,style,value))
}

impl FormDocument {
    pub fn parse(session: &EditSession) -> Result<Self, String> {
        if session.source.len() > MAX_SOURCE { return Err("Form filling is limited to source PDFs of 64 MiB.".into()); }
        session.overlay_guard()?;
        if session.plan.iter().enumerate().any(|(index, page)| page.source != index || page.turns != 0 || page.crop.is_some() || !page.notes.is_empty()) { return Err("Form filling requires the original page plan. Open a fresh copy before filling fields.".into()); }
        let document = Document::load_mem_with_options(&session.source, LoadOptions::with_max_decompressed_size(MAX_TEXT)).map_err(|_| "The PDF cannot be parsed within the form-loading limits.")?;
        if document.is_encrypted() || document.encryption_state.is_some() { return Err("Encrypted PDFs cannot be filled in this build.".into()); }
        if document.objects.len() > 50000 { return Err("This PDF exceeds the form object-count limit.".into()); }
        check_faithful_load(&document, &session.source)?;
        let catalog = document.catalog().map_err(|_| "The PDF catalog is invalid.")?;
        if catalog.has(b"Perms") { return Err("Signed or certified PDFs cannot be filled in this build.".into()); }
        let mut nodes = 0;
        fn safe(value: &Object, nodes: &mut usize, depth: usize) -> Result<(), String> {
            *nodes += 1; if *nodes > 200000 || depth > 32 { return Err("The form document exceeds traversal limits.".into()); }
            match value { Object::Dictionary(dict) => { if [b"A".as_slice(), b"AA", b"OpenAction", b"JS", b"JavaScript", b"ByteRange", b"XFA", b"CO"].iter().any(|key| dict.has(key)) || dict.get(b"Type").and_then(Object::as_name).ok() == Some(b"Sig") || dict.get(b"FT").and_then(Object::as_name).ok() == Some(b"Sig") { return Err("Forms with signatures, actions, calculations, or XFA are not supported.".into()); } for (_, child) in dict { safe(child, nodes, depth + 1)?; } }, Object::Array(values) => for child in values { safe(child, nodes, depth + 1)?; }, Object::Stream(stream) => safe(&Object::Dictionary(stream.dict.clone()), nodes, depth + 1)?, _ => {} } Ok(())
        }
        for value in document.objects.values() { safe(value, &mut nodes, 0)?; }
        let mut pages = Vec::new(); let mut seen = HashSet::new();
        fn page_tree(doc: &Document, id: ObjectId, parent: Option<ObjectId>, pages: &mut Vec<ObjectId>, seen: &mut HashSet<ObjectId>, depth: usize) -> Result<(), String> {
            if depth > 32 || seen.len() >= 8192 || !seen.insert(id) { return Err("The page tree is cyclic, repeated, or too large for form filling.".into()); }
            let value = doc.get_dictionary(id).map_err(|_| "The form page tree is invalid.")?;
            if let Some(parent) = parent { if value.get(b"Parent").and_then(Object::as_reference).ok() != Some(parent) { return Err("The form page parent mapping is invalid.".into()); } }
            match value.get(b"Type").and_then(Object::as_name).ok() { Some(b"Page") => { pages.push(id); if pages.len() > 4096 { return Err("Form filling is limited to 4,096 pages.".into()); } }, Some(b"Pages") => { let kids = value.get(b"Kids").and_then(Object::as_array).map_err(|_| "The form page children are invalid.")?; if kids.len() > 4096 { return Err("The form page child list is too large.".into()); } let before = pages.len(); for kid in kids { page_tree(doc, kid.as_reference().map_err(|_| "The form page child is invalid.")?, Some(id), pages, seen, depth + 1)?; } if value.get(b"Count").and_then(Object::as_i64).ok() != Some((pages.len() - before) as i64) { return Err("The form page count is inconsistent.".into()); } }, _ => return Err("The form page type is invalid.".into()) } Ok(())
        }
        page_tree(&document, catalog.get(b"Pages").and_then(Object::as_reference).map_err(|_| "The form page root is invalid.")?, None, &mut pages, &mut seen, 0)?;
        if pages.len() != session.plan.len() { return Err("The form page count disagrees with the open document.".into()); }
        let Some(form) = catalog.get(b"AcroForm").ok() else {
            for page in &pages { if let Ok(annots) = document.get_dictionary(*page).map_err(|_| "Invalid form page.")?.get(b"Annots") {
                let annots = object(&document, annots)?.as_array().map_err(|_| "The page annotation list is invalid.")?;
                if annots.len() > MAX_FIELDS { return Err("The page annotation list exceeds the form inspection limit.".into()); }
                for annotation in annots { if dict(&document, annotation)?.get(b"Subtype").and_then(Object::as_name).ok() == Some(b"Widget") { return Err("A page widget has no canonical AcroForm field tree.".into()); } }
            } }
            return Ok(Self { document, fields: Vec::new(), pages: pages.len() });
        };
        let form = dict(&document, form)?; keys(form, &[b"Fields", b"DA", b"DR", b"NeedAppearances"])?;
        if form.get(b"NeedAppearances").ok().is_some_and(|value| value.as_bool().ok() != Some(false)) { return Err("Forms requiring viewer-generated appearances are not supported.".into()); }
        let roots = object(&document, form.get(b"Fields").map_err(|_| "The form field tree is missing.")?)?.as_array().map_err(|_| "The form field tree is invalid.")?;
        if roots.len() > MAX_FIELDS { return Err("Form filling is limited to 256 fields.".into()); }
        let mut widgets = HashMap::new();
        for (page, id) in pages.iter().enumerate() { if let Ok(annots) = document.get_dictionary(*id).map_err(|_| "Invalid form page.")?.get(b"Annots") { let annots = object(&document, annots)?.as_array().map_err(|_| "The form widget list is invalid.")?; if annots.len() > MAX_FIELDS { return Err("The form widget list is too large.".into()); } for annot in annots { let id = annot.as_reference().map_err(|_| "Only referenced form widgets are supported.")?; if widgets.insert(id, page).is_some() { return Err("Repeated or ambiguous form widgets are not supported.".into()); } if widgets.len() > MAX_FIELDS { return Err("Form filling is limited to 256 total page widgets.".into()); } } } }
        let mut fields = Vec::new(); let mut names = HashSet::new(); let mut ids = HashSet::new();let mut consumed_widgets=HashSet::new(); let mut text_bytes = 0; let mut ap_bytes = 0;let mut option_count=0;
        for reference in roots {
            let id = reference.as_reference().map_err(|_| "Only flat referenced form fields are supported.")?;
            if !ids.insert(id) { return Err("Repeated form field references are not supported.".into()); }
            let field = document.get_dictionary(id).map_err(|_| "The form field is invalid.")?;
            if field.has(b"Kids") && field.get(b"FT").and_then(Object::as_name).ok()==Some(b"Btn") {
                let name=text(field.get(b"T").map_err(|_|"The radio field name is missing.")?)?;
                if name.trim().is_empty() || name.len()>1024 || name.chars().any(char::is_control) || !names.insert(name.clone()){return Err("Unnamed or duplicate radio fields are unsupported.".into());}
                let(page,radio,value)=radio_group(&document,field,id,&widgets,&pages,&mut consumed_widgets,&mut ap_bytes)?;
                option_count+=radio.options.len();if option_count>256{return Err("Form filling is limited to 256 radio and choice options in total.".into());}
                text_bytes+=name.len()+value.len()+radio.options.iter().map(|option|option.label.len()).sum::<usize>();if text_bytes>MAX_TEXT{return Err("The form names, options and values exceed 1 MiB.".into());}
                fields.push(ParsedField {dto:FormField {field_id:format!("field-{}-{}",id.0,id.1),name,page,value,max_length:None,checked:None,on_state:None,radio:Some(radio),choice:None},object:id,style:None,on_state:None});continue;
            }
            if !consumed_widgets.insert(id){return Err("A form widget is repeated or shared between fields.".into());}
            let is_checkbox=field.get(b"FT").and_then(Object::as_name).ok()==Some(b"Btn");
            let is_choice=field.get(b"FT").and_then(Object::as_name).ok()==Some(b"Ch");
            if is_checkbox { keys(field,&[b"Type",b"Subtype",b"FT",b"T",b"TU",b"V",b"DV",b"AS",b"F",b"Ff",b"Rect",b"P",b"AP",b"BS",b"MK",b"H"])?; }
            else if is_choice{keys(field,&[b"Type",b"Subtype",b"FT",b"T",b"TU",b"V",b"DV",b"F",b"Ff",b"Rect",b"P",b"DA",b"AP",b"BS",b"MK",b"Q",b"Opt",b"I",b"TI"])?;}
            else { keys(field, &[b"Type", b"Subtype", b"FT", b"T", b"TU", b"V", b"DV", b"F", b"Ff", b"Rect", b"P", b"DA", b"AP", b"BS", b"MK", b"MaxLen", b"Q"])?; }
            if field.get(b"Subtype").and_then(Object::as_name).ok() != Some(b"Widget") || !is_checkbox&&!is_choice && field.get(b"FT").and_then(Object::as_name).ok() != Some(b"Tx") { return Err("Only supported text, checkbox, and choice fields with one merged widget are accepted.".into()); }
            if field.get(b"Type").ok().is_some_and(|value| value.as_name().ok() != Some(b"Annot")) { return Err("The form widget annotation type is invalid.".into()); }
            if field.get(b"F").and_then(Object::as_i64).ok() != Some(4) || !is_choice&&field.get(b"Ff").ok().is_some_and(|value| !matches!(value.as_i64().ok(), Some(0 | 2))) || field.get(b"Q").ok().is_some_and(|value| value.as_i64().ok() != Some(0)) { return Err("The form flags or text alignment are unsupported.".into()); }
            let page = *widgets.get(&id).ok_or("The canonical form field has no unique page widget.")?;
            if field.get(b"P").and_then(Object::as_reference).ok() != Some(pages[page]) { return Err("The form widget page relationship is inconsistent.".into()); }
            let name = text(field.get(b"T").map_err(|_| "An unnamed form field is unsupported.")?)?;
            if name.trim().is_empty() || name.len() > 1024 || name.chars().any(char::is_control) || !names.insert(name.clone()) { return Err("Unnamed, duplicate, or unsupported field names are not supported.".into()); }
            if is_checkbox {
                let (on_state,checked)=checkbox(&document,field,&mut ap_bytes)?;
                text_bytes+=name.len(); if text_bytes>MAX_TEXT {return Err("The form text exceeds 1 MiB.".into());}
                fields.push(ParsedField {dto:FormField {field_id:format!("field-{}-{}",id.0,id.1),name,page,value:checked.to_string(),max_length:None,checked:Some(checked),on_state:Some(String::from_utf8(on_state.clone()).map_err(|_|"Invalid checkbox state name.")?),radio:None,choice:None},object:id,style:None,on_state:Some(on_state)});
                continue;
            }
            if is_choice{let(choice,style,value)=choice_field(&document,form,field,id,&mut ap_bytes)?;option_count+=choice.options.len();if option_count>256{return Err("Form filling is limited to 256 radio and choice options in total.".into());}text_bytes+=name.len()+value.len()+choice.options.iter().map(|option|option.label.len()+option.export.len()).sum::<usize>();if text_bytes>MAX_TEXT{return Err("The form names, options and values exceed 1 MiB.".into());}fields.push(ParsedField {dto:FormField {field_id:format!("field-{}-{}",id.0,id.1),name,page,value,max_length:None,checked:None,on_state:None,radio:None,choice:Some(choice)},object:id,style:Some(style),on_state:None});continue;}
            let value = field.get(b"V").ok().map(text).transpose()?.unwrap_or_default(); latin(&value)?;
            if let Ok(default) = field.get(b"DV") { latin(&text(default)?)?; }
            text_bytes += name.len() + value.len(); if text_bytes > MAX_TEXT { return Err("The form text exceeds 1 MiB.".into()); }
            let max_length = field.get(b"MaxLen").ok().map(|value| value.as_i64().map_err(|_| "The form MaxLength is invalid.").and_then(|value| usize::try_from(value).map_err(|_| "The form MaxLength is invalid."))).transpose()?;
            if max_length.is_some_and(|value| value == 0 || value > i32::MAX as usize) { return Err("The form MaxLength is unsupported.".into()); }
            let style = style(&document, form, field)?;
            validate_text_appearance(&document,field,&style,&appearance(&style,&value),!value.is_empty(),&mut ap_bytes)?;
            let parsed = ParsedField { dto: FormField { field_id: format!("field-{}-{}", id.0, id.1), name, page, value, max_length, checked:None, on_state:None, radio:None,choice:None }, object: id, style:Some(style), on_state:None }; fits(&parsed, &parsed.dto.value)?; fields.push(parsed);
        }
        if consumed_widgets.len() != widgets.len() { return Err("The form contains orphaned or foreign page annotations.".into()); }
        Ok(Self { document, fields, pages: pages.len() })
    }
    pub fn query(session: &EditSession, id: u64) -> FormFields {
        let (fields, reason) = match Self::parse(session) { Ok(form) => (form.fields.into_iter().map(|field| field.dto).collect(), None), Err(reason) => (Vec::new(), Some(reason)) };
        FormFields { document_id: id, revision: session.revision, status: if reason.is_none() { "supported" } else { "unsupported" }, reason, input: "printable-latin1", value_byte_limit: MAX_VALUE, fields }
    }
    pub fn prepare(mut self, values: &[FieldValue]) -> Result<(Vec<u8>, Vec<FormField>), String> {
        if values.is_empty() || values.len() > MAX_FIELDS { return Err("Change between one and 256 form fields before saving a copy.".into()); }
        let mut seen = HashSet::new(); let mut changed = HashSet::new();
        for patch in values {
            if !seen.insert(patch.field_id()) { return Err("A form field was submitted more than once.".into()); }
            let field=self.fields.iter_mut().find(|field|field.dto.field_id==patch.field_id()).ok_or("A form field no longer exists. Refresh the field list.")?;
            let different=match patch {
                FieldValue::Text {value,..}=>{fits(field,value)?;let different=*value!=field.dto.value;field.dto.value=value.clone();different},
                FieldValue::Checkbox {checked,..}=>{let current=field.dto.checked.ok_or("The submitted form patch has the wrong field kind.")?;field.dto.checked=Some(*checked);field.dto.value=checked.to_string();current!=*checked},
                FieldValue::Radio {option_id,..}=>{let radio=field.dto.radio.as_mut().ok_or("The submitted form patch has the wrong field kind.")?;let option_id=option_id.as_ref().ok_or("A radio group selection cannot be cleared: the form author requires one option to stay selected.")?;let option=radio.options.iter().find(|option|&option.option_id==option_id).ok_or("The radio option no longer exists. Refresh the field list.")?;let different=radio.selected_option_id.as_ref()!=Some(option_id);radio.selected_option_id=Some(option_id.clone());field.dto.value=option.label.clone();different},
                FieldValue::Choice {option_id:None,..}=>{let choice=field.dto.choice.as_mut().filter(|choice|!choice.multiple).ok_or("The submitted form patch has the wrong field kind.")?;let different=choice.selected_option_id.is_some()||!choice.text.is_empty();choice.selected_option_id=None;choice.text.clear();field.dto.value=String::new();different},
                FieldValue::Choices {option_ids,..}=>{let choice=field.dto.choice.as_mut().filter(|choice|choice.multiple).ok_or("The submitted form patch has the wrong field kind.")?;let mut wanted:Vec<&String>=Vec::new();for id in option_ids{if !choice.options.iter().any(|option|&option.option_id==id){return Err("The choice option no longer exists. Refresh the field list.".into());}if wanted.contains(&id){return Err("A choice option was selected twice.".into());}wanted.push(id);}let ordered:Vec<String>=choice.options.iter().filter(|option|wanted.contains(&&option.option_id)).map(|option|option.option_id.clone()).collect();let different=ordered!=choice.selected_option_ids;choice.selected_option_ids=ordered;field.dto.value=choice.options.iter().filter(|option|choice.selected_option_ids.contains(&option.option_id)).map(|option|option.export.as_str()).collect::<Vec<_>>().join("\n");different},
                FieldValue::Combo {text:typed,..}=>{let style=field.style.as_ref().ok_or("The submitted form patch has the wrong field kind.")?;let choice=field.dto.choice.as_mut().filter(|choice|choice.editable).ok_or("The submitted form patch has the wrong field kind.")?;let matched=choice.options.iter().find(|option|option.label==*typed).map(|option|option.option_id.clone());let(selected,custom)=if typed.is_empty(){(None,String::new())}else if matched.is_some(){(matched,String::new())}else{if choice.options.iter().any(|option|option.export==*typed){return Err("That text is the stored value of an option. Type its label or pick it from the list.".into());}fits_style(style,typed,None)?;(None,typed.clone())};let different=choice.selected_option_id!=selected||choice.text!=custom;choice.selected_option_id=selected;choice.text=custom;field.dto.value=match choice.options.iter().find(|option|Some(&option.option_id)==choice.selected_option_id.as_ref()){Some(option)=>option.export.clone(),None=>choice.text.clone()};different},
                FieldValue::Choice {option_id:Some(option_id),..}=>{let choice=field.dto.choice.as_mut().filter(|choice|!choice.multiple).ok_or("The submitted form patch has the wrong field kind.")?;let option=choice.options.iter().find(|option|&option.option_id==option_id).ok_or("The choice option no longer exists. Refresh the field list.")?;let different=choice.selected_option_id.as_ref()!=Some(option_id)||!choice.text.is_empty();choice.selected_option_id=Some(option_id.clone());choice.text.clear();field.dto.value=option.export.clone();if choice.presentation=="list"{if let Some(style)=field.style.as_mut(){let slots=list_slots(style);let index=choice.options.iter().position(|option|&option.option_id==option_id).unwrap_or(0);if slots>0&&choice.options.len()>slots&&(index<style.top||index>=style.top+slots){style.top=index.min(choice.options.len()-slots);}}}different},
            };
            if different{changed.insert(field.dto.field_id.clone());}
        }
        if changed.is_empty() { return Err("Change at least one form value before saving a new copy.".into()); }
        if self.fields.iter().map(|field| field.dto.name.len() + field.dto.value.len()+field.dto.radio.as_ref().map(|radio|radio.options.iter().map(|option|option.label.len()).sum::<usize>()).unwrap_or(0)+field.dto.choice.as_ref().map(|choice|choice.options.iter().map(|option|option.label.len()+option.export.len()).sum::<usize>()).unwrap_or(0)).sum::<usize>() > MAX_TEXT { return Err("The form names, options and values exceed 1 MiB.".into()); }
        for field in &self.fields {
            if !changed.contains(field.dto.field_id.as_str()) { continue; }
            if let Some(radio)=&field.dto.radio {
                let selected=radio.options.iter().find(|option|Some(&option.option_id)==radio.selected_option_id.as_ref()).ok_or("A radio patch must select one existing option.")?;
                self.document.get_dictionary_mut(field.object).map_err(|_|"The radio parent disappeared during preparation.")?.set("V",Object::Name(selected.label.as_bytes().to_vec()));
                for option in &radio.options {self.document.get_dictionary_mut(option.object).map_err(|_|"The radio widget disappeared during preparation.")?.set("AS",Object::Name(if option.option_id==selected.option_id {option.label.as_bytes().to_vec()}else{b"Off".to_vec()}));}continue;
            }
            if let Some(checked)=field.dto.checked {
                let state=Object::Name(if checked {field.on_state.clone().ok_or("Missing checkbox on state.")?} else {b"Off".to_vec()});
                let widget=self.document.get_dictionary_mut(field.object).map_err(|_| "The checkbox disappeared during preparation.")?;widget.set("V",state.clone());widget.set("AS",state);
                continue;
            }
            let style=field.style.as_ref().ok_or("Missing form text style.")?;
            let content = if let Some(choice)=&field.dto.choice{choice_appearance(style,choice)?}else{appearance(style,&field.dto.value)}.encode().map_err(|_| "Could not encode the form appearance.")?;
            let resources = dictionary! { "Font" => dictionary! { style.font.as_bytes() => style.font_object.clone() }, "ProcSet" => vec![Object::Name(b"PDF".to_vec()), Object::Name(b"Text".to_vec())] };
            let ap = self.document.add_object(Stream::new(dictionary! { "Type" => "XObject", "Subtype" => "Form", "FormType" => 1, "BBox" => real(&[0.0,0.0,style.width,style.height]), "Matrix" => real(&[1.0,0.0,0.0,1.0,0.0,0.0]), "Resources" => resources }, content));
            let widget = self.document.get_dictionary_mut(field.object).map_err(|_| "The form field disappeared during preparation.")?; widget.set("AP", dictionary! { "N" => ap });
            if let Some(choice)=&field.dto.choice{
                let selected=selected_indices(choice);
                if choice.multiple{match selected.len(){0=>{widget.remove(b"V");widget.remove(b"I");},1=>{widget.set("V",pdf_text(&choice.options[selected[0]].export));widget.set("I",vec![Object::Integer(selected[0] as i64)]);},_=>{widget.set("V",selected.iter().map(|index|pdf_text(&choice.options[*index].export)).collect::<Vec<_>>());widget.set("I",selected.iter().map(|index|Object::Integer(*index as i64)).collect::<Vec<_>>());}}continue;}
                if let Some(index)=selected.first(){widget.set("V",pdf_text(&choice.options[*index].export));widget.set("I",vec![Object::Integer(*index as i64)]);if choice.presentation=="list"&&choice.options.len()>list_slots(style){widget.set("TI",Object::Integer(style.top as i64));}}
                else if !choice.text.is_empty(){widget.set("V",pdf_text(&choice.text));widget.remove(b"I");}
                else{widget.remove(b"V");widget.remove(b"I");}
                continue;
            }
            widget.set("V", pdf_text(&field.dto.value));
        }
        let live = reachable(&self.document); self.document.objects.retain(|id, _| live.contains(id));
        let mut bytes = Vec::new(); self.document.save_to(&mut bytes).map_err(|_| "Could not serialize the filled form.")?; if bytes.len() > MAX_OUTPUT { return Err("The filled form output exceeds 256 MiB.".into()); } Ok((bytes, self.fields.into_iter().map(|field| field.dto).collect()))
    }
}

#[cfg(test)]
#[path = "forms_h2_tests.rs"]
mod h2_tests;
#[cfg(test)]
#[path = "forms_breadth_tests.rs"]
mod breadth_tests;
#[cfg(test)]
#[path = "forms_multi_combo_tests.rs"]
pub(crate) mod multi_combo_tests;
