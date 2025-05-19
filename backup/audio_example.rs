use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Data, Device, FromSample, Sample, SampleFormat, Stream, StreamConfig};
use ringbuf::traits::{Producer, Split, Consumer};
use ringbuf::HeapRb;

// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

fn get_audio(input: Device, config: cpal::StreamConfig) -> Stream{
    let input_stream = input
        .build_input_stream(
            &config,
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                // Process audio data here
                println!("Received audio data: {:?}", data);
            },
            move |err| {
                eprintln!("Error occurred on input stream: {}", err);
            },
            None
        )
        .expect("Failed to build input stream");
    input_stream.play().expect("Failed to play input stream");
    println!("Input stream started");
    input_stream
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let host = cpal::default_host();
    let input_device = host.default_input_device().expect("No input device available");
    let output_device = host.default_output_device().expect("No output device available");

    let config: StreamConfig = input_device.default_input_config().expect("No input format available").into();

    println!("Default Input Device: {}", input_device.name().unwrap());
    println!("Default Output Device: {}", output_device.name().unwrap());
    println!("Input Config: {:?}", config);

    let latency = 50.0; // in milliseconds
    let latency_frames = (latency / 1_000.0) * config.sample_rate.0 as f32;
    let latency_samples = latency_frames as usize * config.channels as usize;

    let ring = HeapRb::<f32>::new(latency_samples * 2);
    let (mut producer, mut consumer) = ring.split();

    // Fill the samples with 0.0 equal to the length of the delay.
    for _ in 0..latency_samples {
        // The ring buffer has twice as much space as necessary to add latency here,
        // so this should never fail
        producer.try_push(0.0).unwrap();
    }
    let input_data_fn = move |data: &[f32], _: &cpal::InputCallbackInfo| {
        let mut output_fell_behind = false;
        for &sample in data {
            if producer.try_push(sample).is_err() {
                output_fell_behind = true;
            }
        }
        if output_fell_behind {
            eprintln!("output stream fell behind: try increasing latency");
        }
    };
    let output_data_fn = move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
        let mut input_fell_behind = false;
        for sample in data {
            *sample = match consumer.try_pop() {
                Some(s) => s,
                None => {
                    input_fell_behind = true;
                    0.0
                }
            };
        }
        if input_fell_behind {
            eprintln!("input stream fell behind: try increasing latency");
        }
    };

    // Build streams.
    println!(
        "Attempting to build both streams with f32 samples and `{:?}`.",
        config
    );
    let input_stream = input_device.build_input_stream(&config, input_data_fn, err_fn, None).expect("Failed to build input stream");
    let output_stream = output_device.build_output_stream(&config, output_data_fn, err_fn, None).expect("Failed to build output stream");
    println!("Successfully built streams.");

    // Play the streams.
    println!(
        "Starting the input and output streams with `{}` milliseconds of latency.",
        latency
    );
    input_stream.play().expect("Failed to play input stream");
    output_stream.play().expect("Failed to play output stream");
    println!("Successfully started the streams.");

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![greet])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn err_fn(err: cpal::StreamError) {
    eprintln!("an error occurred on stream: {}", err);
}