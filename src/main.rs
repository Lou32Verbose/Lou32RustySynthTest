use clap::Parser;
use itertools::Itertools;
use rustysynth::MidiFile;
use rustysynth::MidiFileSequencer;
use rustysynth::SoundFont;
use rustysynth::Synthesizer;
use rustysynth::SynthesizerSettings;
use std::collections::HashMap;
use std::fs::File;
use std::sync::{Arc, Mutex};
use tinyaudio::prelude::*;

// CC state per channel
#[derive(Clone, Debug, Default)]
struct ChannelCcState {
    volume: Option<u8>,
    pan: Option<u8>,
    reverb: Option<u8>,
    chorus: Option<u8>,
    modulation: Option<u8>,
    expression: Option<u8>,
    sustain: Option<u8>,
}

// Global CC state manager
struct CcStateManager {
    channels: HashMap<i32, ChannelCcState>,
    global_defaults: ChannelCcState,
}

impl CcStateManager {
    fn new() -> Self {
        Self {
            channels: HashMap::new(),
            global_defaults: ChannelCcState::default(),
        }
    }

    // Set a CC value for a specific channel
    fn set_channel_cc(&mut self, channel: i32, cc_type: &str, value: u8) {
        let channel_state = self.channels.entry(channel).or_insert_with(ChannelCcState::default);
        match cc_type {
            "volume" => channel_state.volume = Some(value),
            "pan" => channel_state.pan = Some(value),
            "reverb" => channel_state.reverb = Some(value),
            "chorus" => channel_state.chorus = Some(value),
            "modulation" => channel_state.modulation = Some(value),
            "expression" => channel_state.expression = Some(value),
            "sustain" => channel_state.sustain = Some(value),
            _ => {}
        }
    }

    // Set global default (applied to all channels)
    fn set_global_cc(&mut self, cc_type: &str, value: u8) {
        match cc_type {
            "volume" => self.global_defaults.volume = Some(value),
            "pan" => self.global_defaults.pan = Some(value),
            "reverb" => self.global_defaults.reverb = Some(value),
            "chorus" => self.global_defaults.chorus = Some(value),
            "modulation" => self.global_defaults.modulation = Some(value),
            "expression" => self.global_defaults.expression = Some(value),
            "sustain" => self.global_defaults.sustain = Some(value),
            _ => {}
        }
    }

    // Get CC value for a channel (channel-specific or global default)
    fn get_cc_value(&self, channel: i32, cc_type: &str) -> Option<u8> {
        if let Some(channel_state) = self.channels.get(&channel) {
            match cc_type {
                "volume" => channel_state.volume,
                "pan" => channel_state.pan,
                "reverb" => channel_state.reverb,
                "chorus" => channel_state.chorus,
                "modulation" => channel_state.modulation,
                "expression" => channel_state.expression,
                "sustain" => channel_state.sustain,
                _ => None,
            }
        } else {
            None
        }
        .or_else(|| {
            match cc_type {
                "volume" => self.global_defaults.volume,
                "pan" => self.global_defaults.pan,
                "reverb" => self.global_defaults.reverb,
                "chorus" => self.global_defaults.chorus,
                "modulation" => self.global_defaults.modulation,
                "expression" => self.global_defaults.expression,
                "sustain" => self.global_defaults.sustain,
                _ => None,
            }
        })
    }

    // Get all channels that have any CC values set
    fn get_active_channels(&self) -> Vec<i32> {
        let mut channels: Vec<i32> = self.channels.keys().copied().collect();
        if !channels.is_empty() {
            channels.sort();
            channels
        } else {
            // If no channel-specific values, return all 16 channels for global defaults
            (0..16).collect()
        }
    }
}

#[derive(Parser, Debug)]
#[command(name = "rustysynthplayer")]
#[command(about = "A MIDI file player using RustySynth")]
struct Args {
    /// Path to the SoundFont file (.sf2)
    soundfont: String,
    
    /// Path to the MIDI file (.mid)
    midi_file: String,
    
    /// Pan position (0-127, 64=center) [default: 64]
    #[arg(long, value_name = "VALUE", value_parser = clap::value_parser!(u8).range(0..=127))]
    pan: Option<u8>,
    
    /// Reverb send level (0-127) [default: 0]
    #[arg(long, value_name = "VALUE", value_parser = clap::value_parser!(u8).range(0..=127))]
    reverb: Option<u8>,
    
    /// Chorus send level (0-127) [default: 0]
    #[arg(long, value_name = "VALUE", value_parser = clap::value_parser!(u8).range(0..=127))]
    chorus: Option<u8>,
    
    /// Volume (0-127) [default: 100]
    #[arg(long, value_name = "VALUE", value_parser = clap::value_parser!(u8).range(0..=127))]
    volume: Option<u8>,
    
    /// Modulation depth (0-127) [default: 0]
    #[arg(long, value_name = "VALUE", value_parser = clap::value_parser!(u8).range(0..=127))]
    modulation: Option<u8>,
    
    /// Expression control (0-127) [default: 127]
    #[arg(long, value_name = "VALUE", value_parser = clap::value_parser!(u8).range(0..=127))]
    expression: Option<u8>,
    
    /// Sustain pedal (on/off) [default: off]
    #[arg(long, value_name = "STATE")]
    sustain: Option<String>,
    
    /// Per-channel parameter: CHANNEL:PARAM:VALUE (e.g., 0:volume:100, 1:pan:50)
    /// Can be specified multiple times. PARAM can be: volume, pan, reverb, chorus, modulation, expression, sustain
    /// Channel numbers are 0-15. For sustain, use 0 or 1 (off/on) instead of 0-127.
    #[arg(long = "channel-param", value_name = "CHANNEL:PARAM:VALUE", num_args = 1..)]
    channel_params: Vec<String>,
}

// MIDI CC message constants
const CC_PAN: i32 = 10;
const CC_REVERB: i32 = 91;
const CC_CHORUS: i32 = 93;
const CC_VOLUME: i32 = 7;
const CC_MODULATION: i32 = 1;
const CC_EXPRESSION: i32 = 11;
const CC_SUSTAIN: i32 = 64;
const MIDI_CC_COMMAND: i32 = 0xB0; // Control Change message

// Send CC messages from state manager to synthesizer
// SAFETY: This function uses unsafe to convert &Synthesizer to &mut Synthesizer.
// This is safe because:
// 1. We hold an exclusive mutex lock on the sequencer
// 2. The sequencer owns the synthesizer, so we have exclusive access
// 3. No other code can access the synthesizer while we hold the lock
// 4. We use raw pointers to avoid the compiler's strict reference casting rules
unsafe fn send_cc_messages_from_state(
    cc_state: &CcStateManager,
    sequencer: &MidiFileSequencer,
) {
    // Get immutable reference to synthesizer
    let synth_ref = sequencer.get_synthesizer();
    
    // Convert to mutable reference via raw pointer
    // This is safe because we have exclusive access via the mutex
    let ptr = synth_ref as *const Synthesizer as *mut Synthesizer;
    #[allow(invalid_reference_casting)]
    let synth_mut = &mut *ptr;
    
    // Get active channels (channels with specific values or all channels for global defaults)
    let active_channels = cc_state.get_active_channels();
    
    // Send CC messages for each active channel
    for &channel in &active_channels {
        // Volume
        if let Some(value) = cc_state.get_cc_value(channel, "volume") {
            synth_mut.process_midi_message(channel, MIDI_CC_COMMAND, CC_VOLUME, value as i32);
        }
        
        // Expression
        if let Some(value) = cc_state.get_cc_value(channel, "expression") {
            synth_mut.process_midi_message(channel, MIDI_CC_COMMAND, CC_EXPRESSION, value as i32);
        }
        
        // Pan
        if let Some(value) = cc_state.get_cc_value(channel, "pan") {
            synth_mut.process_midi_message(channel, MIDI_CC_COMMAND, CC_PAN, value as i32);
        }
        
        // Modulation
        if let Some(value) = cc_state.get_cc_value(channel, "modulation") {
            synth_mut.process_midi_message(channel, MIDI_CC_COMMAND, CC_MODULATION, value as i32);
        }
        
        // Reverb
        if let Some(value) = cc_state.get_cc_value(channel, "reverb") {
            synth_mut.process_midi_message(channel, MIDI_CC_COMMAND, CC_REVERB, value as i32);
        }
        
        // Chorus
        if let Some(value) = cc_state.get_cc_value(channel, "chorus") {
            synth_mut.process_midi_message(channel, MIDI_CC_COMMAND, CC_CHORUS, value as i32);
        }
        
        // Sustain
        if let Some(value) = cc_state.get_cc_value(channel, "sustain") {
            synth_mut.process_midi_message(channel, MIDI_CC_COMMAND, CC_SUSTAIN, value as i32);
        }
    }
}

fn main() {
    let args = Args::parse();
    
    let soundfont_path = &args.soundfont;
    let midi_path = &args.midi_file;

    // Setup the audio output.
    let params = OutputDeviceParameters {
        channels_count: 2,
        sample_rate: 44100,
        channel_sample_count: 4410,
    };

    // Load the SoundFont.
    let mut sf2 = File::open(soundfont_path)
        .unwrap_or_else(|e| {
            eprintln!("Error opening SoundFont file '{}': {}", soundfont_path, e);
            std::process::exit(1);
        });
    let sound_font = Arc::new(SoundFont::new(&mut sf2)
        .unwrap_or_else(|e| {
            eprintln!("Error parsing SoundFont file '{}': {}", soundfont_path, e);
            std::process::exit(1);
        }));

    // Load the MIDI file.
    let mut mid = File::open(midi_path)
        .unwrap_or_else(|e| {
            eprintln!("Error opening MIDI file '{}': {}", midi_path, e);
            std::process::exit(1);
        });
    let midi_file_loaded = MidiFile::new(&mut mid)
        .unwrap_or_else(|e| {
            eprintln!("Error parsing MIDI file '{}': {}", midi_path, e);
            std::process::exit(1);
        });
    let midi_duration_seconds = midi_file_loaded.get_length();
    let midi_file = Arc::new(midi_file_loaded);

    // Create the MIDI file sequencer.
    let settings = SynthesizerSettings::new(params.sample_rate as i32);
    let synthesizer = Synthesizer::new(&sound_font, &settings).unwrap();
    
    // Create CC state manager
    let mut cc_state_manager = CcStateManager::new();
    
    // Apply user-specified parameters as global defaults
    // Default values are applied if not specified
    if let Some(pan) = args.pan {
        cc_state_manager.set_global_cc("pan", pan);
    } else {
        cc_state_manager.set_global_cc("pan", 64);
    }
    
    if let Some(reverb) = args.reverb {
        cc_state_manager.set_global_cc("reverb", reverb);
    } else {
        cc_state_manager.set_global_cc("reverb", 0);
    }
    
    if let Some(chorus) = args.chorus {
        cc_state_manager.set_global_cc("chorus", chorus);
    } else {
        cc_state_manager.set_global_cc("chorus", 0);
    }
    
    if let Some(volume) = args.volume {
        cc_state_manager.set_global_cc("volume", volume);
    } else {
        cc_state_manager.set_global_cc("volume", 100);
    }
    
    if let Some(modulation) = args.modulation {
        cc_state_manager.set_global_cc("modulation", modulation);
    } else {
        cc_state_manager.set_global_cc("modulation", 0);
    }
    
    if let Some(expression) = args.expression {
        cc_state_manager.set_global_cc("expression", expression);
    } else {
        cc_state_manager.set_global_cc("expression", 127);
    }
    
    let sustain_value = match args.sustain.as_deref() {
        Some("on") | Some("ON") => 127,
        Some("off") | Some("OFF") | None => 0,
        Some(val) => {
            eprintln!("Error: Invalid sustain value '{}'. Must be 'on' or 'off'.", val);
            std::process::exit(1);
        }
    };
    cc_state_manager.set_global_cc("sustain", sustain_value);
    
    // Parse per-channel parameters
    for param_str in &args.channel_params {
        // Parse format: CHANNEL:PARAM:VALUE
        let parts: Vec<&str> = param_str.split(':').collect();
        if parts.len() != 3 {
            eprintln!("Error: Invalid channel parameter format '{}'. Expected CHANNEL:PARAM:VALUE", param_str);
            eprintln!("Example: --channel-param 0:volume:100");
            std::process::exit(1);
        }
        
        let channel = match parts[0].parse::<i32>() {
            Ok(ch) if ch >= 0 && ch < 16 => ch,
            Ok(ch) => {
                eprintln!("Error: Channel number must be 0-15, got {}", ch);
                std::process::exit(1);
            }
            Err(e) => {
                eprintln!("Error: Invalid channel number '{}': {}", parts[0], e);
                std::process::exit(1);
            }
        };
        
        let param_type = parts[1].to_lowercase();
        let value_str = parts[2];
        
        // Validate parameter type
        let valid_params = ["volume", "pan", "reverb", "chorus", "modulation", "expression", "sustain"];
        if !valid_params.iter().any(|&p| p == param_type) {
            eprintln!("Error: Invalid parameter type '{}'. Must be one of: {:?}", param_type, valid_params);
            std::process::exit(1);
        }
        
        // Parse value
        if param_type == "sustain" {
            // Sustain is special: accept "on"/"off" or 0/1 or 0-127
            let value = match value_str.to_lowercase().as_str() {
                "on" => 127,
                "off" => 0,
                _ => match value_str.parse::<u8>() {
                    Ok(v) if v >= 64 => 127, // >= 64 means on
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("Error: Invalid sustain value '{}': {}. Use 'on', 'off', or 0-127", value_str, e);
                        std::process::exit(1);
                    }
                }
            };
            cc_state_manager.set_channel_cc(channel, &param_type, value);
        } else {
            // Other parameters: 0-127
            let value = match value_str.parse::<u8>() {
                Ok(v) if v <= 127 => v,
                Ok(v) => {
                    eprintln!("Error: Parameter value must be 0-127, got {}", v);
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("Error: Invalid parameter value '{}': {}", value_str, e);
                    std::process::exit(1);
                }
            };
            cc_state_manager.set_channel_cc(channel, &param_type, value);
        }
    }
    
    let sequencer = MidiFileSequencer::new(synthesizer);

    // Play the MIDI file.
    let sequencer = Arc::new(Mutex::new(sequencer));
    let cc_state = Arc::new(Mutex::new(cc_state_manager));
    
    {
        let mut seq = sequencer.lock().unwrap();
        seq.play(&midi_file, false);
    }

    // Buffer for the audio output.
    let left = Arc::new(Mutex::new(vec![0_f32; params.channel_sample_count]));
    let right = Arc::new(Mutex::new(vec![0_f32; params.channel_sample_count]));

    // Clone references for the closure.
    let sequencer_clone = Arc::clone(&sequencer);
    let left_clone = Arc::clone(&left);
    let right_clone = Arc::clone(&right);
    let cc_state_clone = Arc::clone(&cc_state);

    // Start the audio output.
    let _device = run_output_device(params, {
        move |data| {
            // Lock and render audio.
            let mut seq = sequencer_clone.lock().unwrap();
            
            let mut left_buf = left_clone.lock().unwrap();
            let mut right_buf = right_clone.lock().unwrap();
            
            // Render audio samples (this processes MIDI file events, including CC messages)
            seq.render(&mut left_buf[..], &mut right_buf[..]);
            
            // Send our CC messages AFTER render() to override any MIDI file CC messages
            // This ensures our parameters take precedence
            let cc_state_guard = cc_state_clone.lock().unwrap();
            unsafe {
                send_cc_messages_from_state(&*cc_state_guard, &*seq);
            }
            drop(cc_state_guard);
            
            // Interleave left and right channels.
            for (i, value) in left_buf.iter().interleave(right_buf.iter()).enumerate() {
                data[i] = *value;
            }
        }
    })
    .unwrap();

    // Wait for the MIDI file to finish playing.
    // Calculate duration: MIDI file length in seconds
    std::thread::sleep(std::time::Duration::from_secs_f64(midi_duration_seconds));
}
