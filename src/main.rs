use itertools::Itertools;
use rustysynth::MidiFile;
use rustysynth::MidiFileSequencer;
use rustysynth::SoundFont;
use rustysynth::Synthesizer;
use rustysynth::SynthesizerSettings;
use std::env;
use std::fs::File;
use std::sync::{Arc, Mutex};
use tinyaudio::prelude::*;

fn main() {
    let args: Vec<String> = env::args().collect();
    
    if args.len() != 3 {
        eprintln!("Usage: {} <soundfont.sf2> <midi_file.mid>", args[0]);
        eprintln!("Example: {} TimGM6mb.sf2 flourish.mid", args[0]);
        std::process::exit(1);
    }
    
    let soundfont_path = &args[1];
    let midi_path = &args[2];

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
    let sequencer = MidiFileSequencer::new(synthesizer);

    // Play the MIDI file.
    let sequencer = Arc::new(Mutex::new(sequencer));
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

    // Start the audio output.
    let _device = run_output_device(params, {
        move |data| {
            // Lock and render audio.
            let mut seq = sequencer_clone.lock().unwrap();
            let mut left_buf = left_clone.lock().unwrap();
            let mut right_buf = right_clone.lock().unwrap();
            
            // Render audio samples.
            seq.render(&mut left_buf[..], &mut right_buf[..]);
            
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
