use anyhow::{anyhow, Context};
use once_cell::sync::Lazy;
use parking_lot::{MappedMutexGuard, Mutex, MutexGuard};
use serde::{Deserialize, Deserializer};
use std::any::{Any, TypeId};
use std::collections::HashMap;
use std::sync::Arc;

// --- Start of new code for model_specific_parameters.json ---
const MODEL_SPECIFIC_PARAMETERS_URL: &str = "https://raw.githubusercontent.com/AlgoClaw/Govee/refs/heads/main/decoded/v1.2/model_specific_parameters.json";

#[derive(Deserialize, Debug, Clone)]
pub struct TypeEntry {
    #[allow(dead_code)] // Warning: field `type_entry` is never read
    pub type_entry: u32,
    pub hex_prefix_remove: String,
    pub hex_prefix_add: String,
    pub normal_command_suffix: String,
}

impl Default for TypeEntry {
    fn default() -> Self {
        Self {
            type_entry: 0, 
            hex_prefix_remove: String::new(),
            hex_prefix_add: String::new(), 
            normal_command_suffix: String::new(),
        }
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct ModelSpecificParameter {
    pub models: Vec<String>,
    pub hex_multi_prefix: String,
    pub on_command: bool,
    #[serde(rename = "type")]
    pub type_entries: Vec<TypeEntry>,
}

pub type ModelSpecificParametersCollection = Vec<ModelSpecificParameter>;

static MODEL_SPECIFIC_PARAMS: Lazy<anyhow::Result<ModelSpecificParametersCollection>> =
    Lazy::new(fetch_model_specific_parameters);

fn fetch_model_specific_parameters() -> anyhow::Result<ModelSpecificParametersCollection> {
    let response = reqwest::blocking::get(MODEL_SPECIFIC_PARAMETERS_URL)
        .context("Failed to send request for model specific parameters")?;
    if !response.status().is_success() {
        return Err(anyhow!(
            "Failed to download model specific parameters: HTTP {}",
            response.status()
        ));
    }
    let params: ModelSpecificParametersCollection = response.json()
        .context("Failed to parse model specific parameters JSON")?;
    Ok(params)
}

#[allow(dead_code)] // Warning: function `get_model_specific_parameters` is never used
pub fn get_model_specific_parameters() -> &'static anyhow::Result<ModelSpecificParametersCollection> {
    &MODEL_SPECIFIC_PARAMS
}

fn find_params_for_sku(sku: &str) -> anyhow::Result<&'static ModelSpecificParameter> {
    let params_collection = MODEL_SPECIFIC_PARAMS.as_ref()
        .map_err(|e| anyhow!("Model specific parameters not loaded: {:?}", e))?;

    // First, try to find the specific SKU
    if let Some(params) = params_collection.iter().find(|p| p.models.contains(&sku.to_string())) {
        return Ok(params);
    }

    // If not found, try to find the "null" SKU as a fallback
    params_collection.iter().find(|p| p.models.contains(&"null".to_string()))
        .ok_or_else(|| anyhow!("Parameters not found for SKU '{}' and no 'null' fallback entry found", sku))
}


// Helper function to convert hex string to bytes
fn hex_string_to_bytes(s: &str) -> anyhow::Result<Vec<u8>> {
    if s.is_empty() {
        return Ok(Vec::new());
    }
    hex::decode(s).map_err(|e| anyhow!("Failed to decode hex string '{}': {}", s, e))
}

// Helper function to convert bytes to hex string (for debugging or matching)
#[allow(dead_code)]
fn bytes_to_hex_string(bytes: &[u8]) -> String {
    hex::encode(bytes)
}

// --- End of new code for model_specific_parameters.json ---

static MGR: Lazy<PacketManager> = Lazy::new(PacketManager::new);

#[derive(Clone, PartialEq, Eq)]
pub struct HexBytes(Vec<u8>);

impl std::fmt::Debug for HexBytes {
    fn fmt(&self, fmt: &mut std::fmt::Formatter) -> std::fmt::Result {
        fmt.write_fmt(format_args!("{:02X?}", self.0))
    }
}

pub struct PacketCodec {
    encode: Box<dyn Fn(&dyn Any) -> anyhow::Result<Vec<u8>> + Sync + Send>,
    decode: Box<dyn Fn(&[u8]) -> anyhow::Result<GoveeBlePacket> + Sync + Send>,
    supported_skus: &'static [&'static str],
    type_id: TypeId,
}

impl PacketCodec {
    pub fn new<T: 'static>(
        supported_skus: &'static [&'static str],
        encode: impl Fn(&T) -> anyhow::Result<Vec<u8>> + 'static + Sync + Send,
        decode: impl Fn(&[u8]) -> anyhow::Result<GoveeBlePacket> + 'static + Sync + Send,
    ) -> Self {
        Self {
            encode: Box::new(move |any| {
                let type_id = TypeId::of::<T>();
                let value = any.downcast_ref::<T>().ok_or_else(|| {
                    anyhow!("cannot downcast to {type_id:?} in PacketCodec encoder")
                })?;
                (encode)(value)
            }),
            decode: Box::new(decode),
            supported_skus,
            type_id: TypeId::of::<T>(),
        }
    }
}

pub struct PacketManager {
    codec_by_sku: Mutex<HashMap<String, HashMap<TypeId, Arc<PacketCodec>>>>,
    all_codecs: Vec<Arc<PacketCodec>>,
}

impl PacketManager {
    fn map_for_sku(&self, sku: &str) -> MappedMutexGuard<HashMap<TypeId, Arc<PacketCodec>>> {
        MutexGuard::map(self.codec_by_sku.lock(), |codecs| {
            codecs.entry(sku.to_string()).or_insert_with(|| {
                let mut map = HashMap::new();
                for codec in &self.all_codecs {
                    if codec.supported_skus.iter().any(|s| *s == sku || *s == "*" ) { // Allow wildcard
                        if map.insert(codec.type_id.clone(), codec.clone()).is_some() {
                            eprintln!("Conflicting PacketCodecs for {sku} {:?}", codec.type_id);
                        }
                    }
                }
                map
            })
        })
    }

    fn resolve_by_sku(&self, sku: &str, type_id: &TypeId) -> anyhow::Result<Arc<PacketCodec>> {
        let map = self.map_for_sku(sku);
        map.get(type_id)
            .cloned()
            .ok_or_else(|| anyhow!("sku {sku} has no codec for type {type_id:?}"))
    }

    pub fn decode_for_sku(&self, sku: &str, data: &[u8]) -> GoveeBlePacket {
        let map = self.map_for_sku(sku);
        for codec in map.values() {
            if let Ok(value) = (codec.decode)(data) {
                return value;
            }
        }
        GoveeBlePacket::Generic(HexBytes(data.to_vec()))
    }

    pub fn encode_for_sku<T: 'static>(&self, sku: &str, value: &T) -> anyhow::Result<Vec<u8>> {
        let type_id = TypeId::of::<T>();
        let codec = self.resolve_by_sku(sku, &type_id)?;
        (codec.encode)(value)
    }

    pub fn new() -> Self {
        if let Err(e) = MODEL_SPECIFIC_PARAMS.as_ref() {
            eprintln!("Failed to load model specific parameters during PacketManager init: {:?}", e);
        }

        let mut all_codecs = vec![];
        macro_rules! encode_body {
            ($target:expr,$input:expr,) => {};
            ($target:expr,$input:expr, $expected:literal, $($tail:tt)*) => {
                $target.push($expected);
                encode_body!($target, $input, $($tail)*);
            };
            ($target:expr, $input:expr, $field_name:ident, $($tail:tt)*) => {
                $input.$field_name.encode_param($target);
                encode_body!($target, $input, $($tail)*);
            };
        }
        macro_rules! decode_body {
            ($target:expr, $data:expr,) => {
                while !$data.is_empty() { anyhow::ensure!($data[0] == 0); $data = &$data[1..]; }
            };
            ($target:expr, $data:expr, $expected:literal, $($tail:tt)*) => {
                let maybe_byte = $data.get(0);
                anyhow::ensure!(maybe_byte == Some(&$expected),"expected {} but got {maybe_byte:?}", $expected);
                $data = &$data[1..];
                decode_body!($target, $data, $($tail)*);
            };
            ($target:expr, $data:expr, $field_name:ident, $($tail:tt)*) => {
                let remain = $target.$field_name.decode_param($data)?;
                $data = remain;
                decode_body!($target, $data, $($tail)*);
            };
        }
        macro_rules! packet {
            ($skus:expr, $struct:ident, $variant:ident, $($body:tt)*) => {
                PacketCodec::new(
                    $skus,
                    |input_value: &$struct| {
                        let mut bytes = vec![];
                        encode_body!(&mut bytes, input_value, $($body)*);
                        Ok(finish(bytes)) // Assumes all these packets are single-line 20-byte commands
                    },
                    |data| {
                        let mut data = &data[0..data.len().saturating_sub(1)];
                        let mut value = $struct::default();
                        decode_body!(&mut value, data, $($body)*);
                        Ok(GoveeBlePacket::$variant(value))
                    }
                )
            }
        }

        all_codecs.push(packet!(&["H7160"], SetHumidifierMode, SetHumidifierMode, 0x33,0x05,mode,param,));
        all_codecs.push(packet!(&["H7160"], NotifyHumidifierMode, NotifyHumidifierMode, 0xaa,0x05,0x00,mode,param,));
        all_codecs.push(packet!(&["H7160"], HumidifierAutoMode, NotifyHumidifierAutoMode, 0xaa,0x05,0x03,target_humidity,));
        all_codecs.push(packet!(&["H7160"], NotifyHumidifierNightlightParams, NotifyHumidifierNightlight, 0xaa,0x1b,on,brightness,r,g,b,));
        all_codecs.push(packet!(&["H7160"], SetHumidifierNightlightParams, SetHumidifierNightlight, 0x33,0x1b,on,brightness,r,g,b,));
        
        all_codecs.push(PacketCodec::new(
            &["*"], 
            |value: &SetSceneCode| value.encode(),
            SetSceneCode::decode,
        ));

        all_codecs.push(packet!(&["Generic:Light","*"], SetDevicePower, SetDevicePower, 0x33,0x01,on,));

        Self {
            codec_by_sku: Mutex::new(HashMap::new()),
            all_codecs: all_codecs.into_iter().map(Arc::new).collect(),
        }
    }
}

pub trait DecodePacketParam {
    fn decode_param<'a>(&mut self, data: &'a [u8]) -> anyhow::Result<&'a [u8]>;
    fn encode_param(&self, target: &mut Vec<u8>);
}

impl DecodePacketParam for u8 {
    fn decode_param<'a>(&mut self, data: &'a [u8]) -> anyhow::Result<&'a [u8]> {
        *self = *data.get(0).ok_or_else(|| anyhow!("EOF for u8"))?;
        Ok(&data[1..])
    }
    fn encode_param(&self, target: &mut Vec<u8>) { target.push(*self); }
}

impl DecodePacketParam for u16 {
    fn decode_param<'a>(&mut self, data: &'a [u8]) -> anyhow::Result<&'a [u8]> {
        let lo = *data.get(0).ok_or_else(|| anyhow!("EOF for u16 lo"))?;
        let hi = *data.get(1).ok_or_else(|| anyhow!("EOF for u16 hi"))?;
        *self = ((hi as u16) << 8) | lo as u16;
        Ok(&data[2..])
    }
    fn encode_param(&self, target: &mut Vec<u8>) {
        let hi = (*self >> 8) as u8;
        let lo = (*self & 0xff) as u8;
        target.push(lo);
        target.push(hi);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct SetHumidifierNightlightParams { pub on: bool, pub r: u8, pub g: u8, pub b: u8, pub brightness: u8, }
impl Into<SetHumidifierNightlightParams> for NotifyHumidifierNightlightParams {
    fn into(self) -> SetHumidifierNightlightParams {
        SetHumidifierNightlightParams { on: self.on, r: self.r, g: self.g, b: self.b, brightness: self.brightness, }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct NotifyHumidifierNightlightParams { pub on: bool, pub r: u8, pub g: u8, pub b: u8, pub brightness: u8, }
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct TargetHumidity(u8);
impl Into<u8> for TargetHumidity { fn into(self) -> u8 { self.0 } }
impl DecodePacketParam for TargetHumidity {
    fn decode_param<'a>(&mut self, data: &'a [u8]) -> anyhow::Result<&'a [u8]> { self.0.decode_param(data) }
    fn encode_param(&self, target: &mut Vec<u8>) { target.push(self.0); }
}
impl TargetHumidity {
    pub fn as_percent(&self) -> u8 { self.0 & 0x7f }
    #[allow(dead_code)] pub fn into_inner(self) -> u8 { self.0 }
    #[allow(dead_code)] pub fn from_percent(percent: u8) -> Self { Self(percent + 128) }
}
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct SetHumidifierMode { pub mode: u8, pub param: u8, }
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct NotifyHumidifierMode { pub mode: u8, pub param: u8, }
#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct HumidifierAutoMode { pub target_humidity: TargetHumidity, }

#[derive(Clone, Debug, PartialEq, Eq)] 
pub struct SetSceneCode {
    code: u16,
    scence_param: String,
    sku: String, 
}

impl SetSceneCode {
    pub fn new(code: u16, scence_param: String, sku: String) -> Self {
        Self { code, scence_param, sku }
    }

    pub fn encode(&self) -> anyhow::Result<Vec<u8>> {
        let model_params = find_params_for_sku(&self.sku)?;

        let mut current_scence_bytes = data_encoding::BASE64.decode(self.scence_param.as_bytes())
            .with_context(|| format!("Failed to decode base64 scence_param: {}", self.scence_param))?;

        let raw_scence_hex_str = bytes_to_hex_string(&current_scence_bytes);
        let matched_type_entry = model_params.type_entries.iter().find(|te| {
            !te.hex_prefix_remove.is_empty() && raw_scence_hex_str.starts_with(&te.hex_prefix_remove)
        }).cloned().unwrap_or_else(|| {
            model_params.type_entries.iter().find(|te| te.hex_prefix_remove.is_empty())
                .cloned()
                .unwrap_or_default() 
        });

        if !matched_type_entry.hex_prefix_remove.is_empty() {
            let remove_bytes = hex_string_to_bytes(&matched_type_entry.hex_prefix_remove)?;
            if current_scence_bytes.starts_with(&remove_bytes) {
                current_scence_bytes = current_scence_bytes[remove_bytes.len()..].to_vec();
            }
        }

        let hex_prefix_add_bytes = hex_string_to_bytes(&matched_type_entry.hex_prefix_add)?;
        let mut data_for_segmentation_payload = hex_prefix_add_bytes;
        data_for_segmentation_payload.extend_from_slice(&current_scence_bytes);
        
        // This is the actual data that needs to be broken into 17-byte payloads,
        // prefixed by 0x01 and num_lines_byte for the first line.
        let mut temp_payload_for_num_lines_calc = vec![0x01]; // Start with 0x01
        // Placeholder for num_lines_byte itself (1 byte)
        temp_payload_for_num_lines_calc.push(0x00); // Placeholder, will be replaced
        temp_payload_for_num_lines_calc.extend(data_for_segmentation_payload.iter().cloned());

        let num_lines_byte = 
            if temp_payload_for_num_lines_calc.is_empty() { // Should not be empty due to 0x01,0x00
                1 
            } else {
                ((temp_payload_for_num_lines_calc.len() + 16) / 17).max(1) as u8
            };

        // Prepare full_payload_for_segmentation with the correct num_lines_byte
        let mut full_payload_for_segmentation = vec![0x01, num_lines_byte];
        full_payload_for_segmentation.extend(data_for_segmentation_payload); // Already has hex_prefix_add

        let mut all_command_lines_data: Vec<Vec<u8>> = Vec::new();
        let hex_multi_prefix_byte = u8::from_str_radix(&model_params.hex_multi_prefix, 16)
            .with_context(|| format!("Invalid hex_multi_prefix: {}", model_params.hex_multi_prefix))?;

        let mut payload_cursor = 0;
        for i in 0..num_lines_byte {
            // Added check to ensure we don't create an empty line if all payload is consumed
            if payload_cursor >= full_payload_for_segmentation.len() && num_lines_byte > 0 {
                 // This case should ideally be avoided by correct num_lines_byte calculation.
                 // If num_lines_byte forced a loop but there's no data for this iteration.
                 if i > 0 { // Only break if it's not the very first (potentially only) line
                    // This might happen if num_lines_byte is slightly off.
                    // For now, let's log if this happens unexpectedly.
                    log::warn!("Payload cursor at end but loop continues, i: {}, num_lines_byte: {}", i, num_lines_byte);
                    break; 
                 }
            }

            let line_index_byte = if num_lines_byte == 1 { 0xff } 
                                  else if i == num_lines_byte - 1 { 0xff } 
                                  else { i };
            
            let mut current_line_data = vec![hex_multi_prefix_byte, line_index_byte];
            
            let chunk_end = (payload_cursor + 17).min(full_payload_for_segmentation.len());
            if payload_cursor < chunk_end { 
                 current_line_data.extend_from_slice(&full_payload_for_segmentation[payload_cursor..chunk_end]);
            }
            payload_cursor = chunk_end;
            all_command_lines_data.push(current_line_data);
        }

        let mut mode_cmd_payload = vec![0x33, 0x05, 0x04];
        mode_cmd_payload.extend_from_slice(&self.code.to_le_bytes()); 
        if !matched_type_entry.normal_command_suffix.is_empty() {
            mode_cmd_payload.extend(hex_string_to_bytes(&matched_type_entry.normal_command_suffix)?);
        }
        all_command_lines_data.push(mode_cmd_payload);

        let mut final_byte_stream: Vec<u8> = Vec::new();
        for line_data in all_command_lines_data {
            final_byte_stream.extend(finish(line_data));
        }
        
        if model_params.on_command {
            let on_cmd_finished = finish(vec![0x33, 0x01, 0x01]);
            let mut temp_stream = on_cmd_finished;
            temp_stream.extend(final_byte_stream);
            final_byte_stream = temp_stream;
        }

        Ok(final_byte_stream)
    }

    pub fn decode(_data: &[u8]) -> anyhow::Result<GoveeBlePacket> {
        anyhow::bail!("SetSceneCode::decode is not implemented");
    }
}

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub struct SetDevicePower { pub on: bool, }

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GoveeBlePacket {
    Generic(HexBytes),
    #[allow(dead_code)] 
    SetSceneCode(SetSceneCode),
    SetDevicePower(SetDevicePower),
    SetHumidifierNightlight(SetHumidifierNightlightParams),
    NotifyHumidifierMode(NotifyHumidifierMode),
    SetHumidifierMode(SetHumidifierMode),
    NotifyHumidifierAutoMode(HumidifierAutoMode),
    NotifyHumidifierNightlight(NotifyHumidifierNightlightParams),
}

#[derive(Debug)]
pub struct Base64HexBytes(HexBytes);

impl Base64HexBytes {
    pub fn decode_for_sku(&self, sku: &str) -> GoveeBlePacket {
        MGR.decode_for_sku(sku, &self.0 .0)
    }

    pub fn encode_for_sku<T: 'static>(sku: &str, value: &T) -> anyhow::Result<Self> {
        MGR.encode_for_sku(sku, value)
            .map(|bytes| Base64HexBytes(HexBytes(bytes)))
    }

    pub fn base64(&self) -> Vec<String> {
        self.0 .0.chunks(20).map(|chunk| data_encoding::BASE64.encode(chunk)).collect()
    }
    
    #[allow(dead_code)]
    pub fn with_bytes(bytes: Vec<u8>) -> Self { 
        Self(HexBytes(finish(bytes)))
    }
}

impl<'de> Deserialize<'de> for Base64HexBytes {
    fn deserialize<D>(deserializer: D) -> Result<Self, <D as Deserializer<'de>>::Error>
    where D: Deserializer<'de>, {
        use serde::de::Error as _;
        let encoded = String::deserialize(deserializer)?;
        let decoded = data_encoding::BASE64.decode(encoded.as_ref())
            .map_err(|e| D::Error::custom(format!("Base64 decode error: {e:#}")))?;
        Ok(Self(HexBytes(decoded)))
    }
}

fn calculate_checksum(data: &[u8]) -> u8 { 
    data.iter().take(19).fold(0, |acc, &x| acc ^ x)
}

fn finish(data: Vec<u8>) -> Vec<u8> { 
    let mut data_to_checksum = data; 
    data_to_checksum.resize(19,0); 
    
    let final_checksum = calculate_checksum(&data_to_checksum); 

    data_to_checksum.push(final_checksum); 
    data_to_checksum
}

impl DecodePacketParam for bool {
    fn decode_param<'a>(&mut self, data: &'a [u8]) -> anyhow::Result<&'a [u8]> {
        let mut byte = 0u8;
        let remain = byte.decode_param(data)?;
        *self = itob(&byte);
        Ok(remain)
    }
    fn encode_param(&self, target: &mut Vec<u8>) { target.push(btoi(*self)); }
}
fn btoi(on: bool) -> u8 { if on { 1 } else { 0 } }
fn itob(i: &u8) -> bool { *i != 0 }

impl GoveeBlePacket {}


#[cfg(test)]
mod test {
    use super::*;
    // It's good practice to initialize logging for tests if your code uses log::warn etc.
    // fn init_log() { let _ = env_logger::builder().is_test(true).try_init(); }


    fn ensure_params_loaded() -> &'static ModelSpecificParametersCollection {
        // init_log(); // Call if logs are needed during tests
        MODEL_SPECIFIC_PARAMS.as_ref().expect("Failed to load model specific parameters for tests")
    }

    #[test]
    fn packet_manager_ops() { 
        ensure_params_loaded();
        assert_eq!(
            MGR.decode_for_sku(
                "H7160",
                &[0x33, 0x05, 0x01, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x17] 
            ),
            GoveeBlePacket::SetHumidifierMode(SetHumidifierMode { mode: 1, param: 0x20 })
        );
        assert_eq!(
            MGR.encode_for_sku( "H7160", &SetHumidifierMode { mode: 1, param: 0x20 }).unwrap(),
            vec![0x33, 0x05, 0x01, 0x20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x17]
        );
    }

    fn round_trip<T: 'static + std::fmt::Debug + PartialEq>(sku: &str, value: &T, expect: GoveeBlePacket) {
        ensure_params_loaded();
        let bytes_container = Base64HexBytes::encode_for_sku(sku, value).unwrap();
        let decoded = bytes_container.decode_for_sku(sku);
        assert_eq!(decoded, expect);
    }

    #[test]
    fn basic_round_trip() {
        ensure_params_loaded();
        round_trip( "Generic:Light", &SetDevicePower { on: true }, GoveeBlePacket::SetDevicePower(SetDevicePower { on: true }), );
        round_trip( "H7160",
            &SetHumidifierNightlightParams { on: true, r: 255, g: 69, b: 42, brightness: 100, },
            GoveeBlePacket::SetHumidifierNightlight(SetHumidifierNightlightParams { on: true, r: 255, g: 69, b: 42, brightness: 100, }),
        );
    }

    #[test]
    fn scene_command_h6065_star() {
        ensure_params_loaded();
        let sku = "H6065";
        let scence_param_b64 = "EgAAAAAnFQ8DAAEFAAgAEokAEokAEon/2DH/2DEAEokAEokAEok=";
        let scene_code = 2899; 

        let command_obj = SetSceneCode::new(scene_code, scence_param_b64.to_string(), sku.to_string());
        let result_bytes = command_obj.encode().unwrap();
        
        let expected_bytes_str = "a30001030427150f03000105000800128900121ea30189001289ffd831ffd83100128900128900b0a3ff1289000000000000000000000000000000c7330504530b00470000000000000000000000002d";
        let expected_bytes = hex_string_to_bytes(expected_bytes_str).unwrap();

        println!("SKU: {}", sku);
        println!("Scene Param (b64): {}", scence_param_b64);
        println!("Scene Code: {}", scene_code);
        println!("Encoded bytes (hex): {}", bytes_to_hex_string(&result_bytes));
        println!("Expected bytes (hex): {}", bytes_to_hex_string(&expected_bytes));
        
        println!("Encoded lines:");
        for (i, chunk) in result_bytes.chunks(20).enumerate() {
            println!("Line {}: {}", i + 1, bytes_to_hex_string(chunk));
        }
        println!("Expected lines:");
         for (i, chunk) in expected_bytes.chunks(20).enumerate() {
            println!("Line {}: {}", i + 1, bytes_to_hex_string(chunk));
        }

        assert_eq!(result_bytes, expected_bytes, "Encoded bytes do not match expected for H6065 Star scene");
    }

    #[test]
    fn scene_command_forest_snapshot() { 
        ensure_params_loaded();
        const FOREST_SCENCE_PARAM: &str = "AyYAAQAKAgH/GQG0CgoCyBQF//8AAP//////AP//lP8AFAGWAAAAACMAAg8FAgH/FAH7AAAB+goEBP8AtP8AR///4/8AAAAAAAAAABoAAAABAgH/BQHIFBQC7hQBAP8AAAAAAAAAAA==";
        const FOREST_SCENE_CODE: u16 = 212; 
        let command = SetSceneCode::new(FOREST_SCENE_CODE, FOREST_SCENCE_PARAM.to_string(), "H619C".to_string()); 
        
        let padded_bytes = command.encode().unwrap();

        println!("data is (Forest Scene - H619C params):");
        let mut hex_output = String::new();
        for (idx, b) in padded_bytes.iter().enumerate() {
            if idx > 0 && idx % 20 == 0 { hex_output.push('\n'); } 
            else if idx > 0 { hex_output.push(' '); }
            hex_output.push_str(&format!("{b:02x}"));
        }
        println!("{hex_output}");

        k9::snapshot!(
            hex_output,
            "
a3 00 01 06 02 03 26 00 01 00 0a 02 01 ff 19 01 b4 0a 0a d9
a3 01 02 c8 14 05 ff ff 00 00 ff ff ff ff ff 00 ff ff 94 12
a3 02 ff 00 14 01 96 00 00 00 00 23 00 02 0f 05 02 01 ff 0a
a3 03 14 01 fb 00 00 01 fa 0a 04 04 ff 00 b4 ff 00 47 ff b3
a3 04 ff e3 ff 00 00 00 00 00 00 00 00 1a 00 00 00 01 02 5d
a3 ff 01 ff 05 01 c8 14 14 02 ee 14 01 00 ff 00 00 00 00 92
33 05 04 d4 00 00 00 00 00 00 00 00 00 00 00 00 00 00 00 e6
"
        );
    }
}

