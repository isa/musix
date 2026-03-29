use std::{
    fs, io,
    path::PathBuf,
    sync::mpsc,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, SetTitle, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Gauge, List, ListItem, ListState, Paragraph},
};
use rodio::{Decoder, OutputStream, Sink, Source};

/// Simple WSOLA (Waveform Similarity Overlap-Add) time-stretcher.
/// Changes playback speed without altering pitch. No FFT, no phase vocoder —
/// just windowed overlap-add with best-correlation search for clean results.
fn time_stretch_wsola(samples: &[f32], channels: u16, rate: f32) -> Vec<f32> {
    let ch = channels as usize;
    if ch == 0 || samples.is_empty() || (rate - 1.0).abs() < 0.001 {
        return samples.to_vec();
    }

    let total_frames = samples.len() / ch;
    let segment_frames = 1024; // ~23ms at 44100 Hz
    let hop_out_frames = segment_frames / 2;
    let hop_in_frames = ((hop_out_frames as f64) * rate as f64) as usize;
    let search_frames = 128; // search range for best overlap

    if hop_in_frames == 0 || total_frames < segment_frames {
        return samples.to_vec();
    }

    // Hann window
    let window: Vec<f32> = (0..segment_frames)
        .map(|i| {
            0.5 * (1.0 - (2.0 * std::f32::consts::PI * i as f32 / segment_frames as f32).cos())
        })
        .collect();

    let output_frames = (total_frames as f64 / rate as f64) as usize + segment_frames;
    let mut output = vec![0.0f32; output_frames * ch];
    let mut norm = vec![0.0f32; output_frames]; // normalization weights per frame

    let mut in_frame: usize = 0;
    let mut out_frame: usize = 0;

    while in_frame + segment_frames <= total_frames && out_frame + segment_frames <= output_frames {
        // WSOLA: find best offset within search range by cross-correlation
        let best_offset = if out_frame >= hop_out_frames && in_frame > 0 {
            let mut best = 0i32;
            let mut best_corr = f32::NEG_INFINITY;
            let range = search_frames as i32;

            for offset in -range..=range {
                let candidate = in_frame as i32 + offset;
                if candidate < 0 || (candidate as usize + segment_frames) > total_frames {
                    continue;
                }
                // Compute correlation over overlap region (first hop_out_frames)
                let mut corr = 0.0f32;
                let overlap_end = out_frame.min(output_frames);
                let overlap_start = out_frame.saturating_sub(hop_out_frames);
                let overlap_len = overlap_end - overlap_start;
                for f in 0..overlap_len.min(hop_out_frames) {
                    let out_idx = overlap_start + f;
                    if out_idx < output_frames && norm[out_idx] > 0.0 {
                        let out_val = output[out_idx * ch] / norm[out_idx];
                        let in_idx = candidate as usize + f;
                        if in_idx < total_frames {
                            corr += out_val * samples[in_idx * ch];
                        }
                    }
                }
                if corr > best_corr {
                    best_corr = corr;
                    best = offset;
                }
            }
            best
        } else {
            0
        };

        let actual_in = (in_frame as i32 + best_offset).max(0) as usize;

        // Overlap-add with Hann window
        for f in 0..segment_frames {
            let w = window[f];
            let of = out_frame + f;
            if of >= output_frames || actual_in + f >= total_frames {
                break;
            }
            for c in 0..ch {
                output[of * ch + c] += samples[(actual_in + f) * ch + c] * w;
            }
            norm[of] += w;
        }

        in_frame = (actual_in + hop_in_frames).min(total_frames);
        out_frame += hop_out_frames;
    }

    // Normalize
    let final_frames = out_frame.min(output_frames);
    for f in 0..final_frames {
        if norm[f] > 0.001 {
            for c in 0..ch {
                output[f * ch + c] /= norm[f];
            }
        }
    }

    output.truncate(final_frames * ch);
    output
}

struct VecSource {
    data: Vec<f32>,
    pos: usize,
    channels: u16,
    sample_rate: u32,
    duration: Option<Duration>,
}

impl VecSource {
    fn new(data: Vec<f32>, channels: u16, sample_rate: u32) -> Self {
        let total_samples = data.len() as u64;
        let channels_u64 = channels as u64;
        let sample_rate_u64 = sample_rate as u64;
        let duration = if channels_u64 > 0 && sample_rate_u64 > 0 {
            Some(Duration::from_secs_f64(total_samples as f64 / (channels_u64 * sample_rate_u64) as f64))
        } else {
            None
        };
        VecSource {
            data,
            pos: 0,
            channels,
            sample_rate,
            duration,
        }
    }
}

impl Iterator for VecSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos < self.data.len() {
            let sample = self.data[self.pos];
            self.pos += 1;
            Some(sample)
        } else {
            None
        }
    }
}

impl Source for VecSource {
    fn current_frame_len(&self) -> Option<usize> {
        Some((self.data.len() - self.pos) / self.channels as usize)
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<Duration> {
        self.duration
    }
}

#[derive(Clone)]
struct Song {
    name: String,
    path: PathBuf,
}

const HIGHLIGHT_COLOR: Color = Color::Rgb(0, 255, 150);
const PRIMARY_COLOR: Color = Color::LightGreen;

struct Player {
    songs: Vec<Song>,
    current_index: usize,
    selected_index: usize,
    _stream: Option<Box<dyn std::any::Any>>,
    _stream_handle: Option<Box<dyn std::any::Any>>,
    sink: Option<Arc<Mutex<Sink>>>,
    is_playing: bool,
    loop_mode: bool,
    random_mode: bool,
    list_state: ListState,
    playback_start: Option<Instant>,
    song_duration: Option<Duration>,
    seek_offset: Duration,
    show_controls_popup: bool,
    search_mode: bool,
    search_query: String,
    filtered_songs: Vec<usize>,
    g_pressed: bool,
    playback_rate: f32,
    decoded_samples: Option<Vec<f32>>,
    decoded_channels: u16,
    decoded_sample_rate: u32,
    stretch_rx: Option<mpsc::Receiver<(Vec<f32>, f32)>>,
    pending_stretched: Option<Vec<f32>>,
    playing_stretched: bool,
}

impl Player {
    fn update_terminal_title(&self) {
        if self.songs.is_empty() {
            return;
        }

        let title = if self.is_playing {
            format!("MUSIX - ♪ {}", self.songs[self.current_index].name)
        } else {
            format!("MUSIX - {} (Paused)", self.songs[self.current_index].name)
        };

        let _ = execute!(io::stdout(), SetTitle(&title));
    }
    fn new(music_dir: Option<PathBuf>) -> Result<Self, Box<dyn std::error::Error>> {
        let songs = load_mp3_files(music_dir)?;
        if songs.is_empty() {
            return Err("No MP3 files found".into());
        }

        let mut list_state = ListState::default();
        list_state.select(Some(0));

        // Initialize audio system with Rodio 0.20 API
        let (stream, stream_handle, sink) = match OutputStream::try_default() {
            Ok((stream, stream_handle)) => match Sink::try_new(&stream_handle) {
                Ok(sink) => (
                    Some(Box::new(stream) as Box<dyn std::any::Any>),
                    Some(Box::new(stream_handle) as Box<dyn std::any::Any>),
                    Some(Arc::new(Mutex::new(sink))),
                ),
                Err(e) => {
                    eprintln!("Warning: Could not create audio sink: {e}");
                    (
                        Some(Box::new(stream) as Box<dyn std::any::Any>),
                        Some(Box::new(stream_handle) as Box<dyn std::any::Any>),
                        None,
                    )
                }
            },
            Err(e) => {
                eprintln!("Warning: Could not initialize audio output: {e}");
                eprintln!("The application will continue but audio playback may not work.");
                (None, None, None)
            }
        };

        let filtered_songs: Vec<usize> = (0..songs.len()).collect();

        let player = Player {
            songs,
            current_index: 0,
            selected_index: 0,
            _stream: stream,
            _stream_handle: stream_handle,
            sink,
            is_playing: false,
            loop_mode: true,
            random_mode: false,
            list_state,
            playback_start: None,
            song_duration: None,
            seek_offset: Duration::from_secs(0),
            show_controls_popup: false,
            search_mode: false,
            search_query: String::new(),
            filtered_songs,
            g_pressed: false,
            playback_rate: 1.0,
            decoded_samples: None,
            decoded_channels: 0,
            decoded_sample_rate: 0,
            stretch_rx: None,
            pending_stretched: None,
            playing_stretched: false,
        };

        // Set initial terminal title
        if !player.songs.is_empty() {
            let _ = execute!(io::stdout(), SetTitle(&format!("MUSIX - {}", player.songs[0].name)));
        } else {
            let _ = execute!(io::stdout(), SetTitle("MUSIX"));
        }

        Ok(player)
    }

    fn play_song(&mut self, index: usize) -> Result<(), Box<dyn std::error::Error>> {
        if index >= self.songs.len() {
            return Ok(());
        }

        self.current_index = index;
        self.selected_index = index;
        self.list_state.select(Some(self.selected_index));
        self.seek_offset = Duration::from_secs(0);

        let should_stretch = (self.playback_rate - 1.0).abs() >= 0.01;

        let sink = self.sink.clone();
        if let Some(sink) = sink {
            let song = &self.songs[index];
            match std::fs::File::open(&song.path) {
                Ok(file) => match Decoder::new(file) {
                    Ok(source) => {
                        let total_duration = source.total_duration();
                        let sr = source.sample_rate();
                        let ch = source.channels();

                        let raw_samples: Vec<f32> = source.convert_samples::<f32>().collect();
                        self.decoded_samples = Some(raw_samples.clone());
                        self.decoded_channels = ch;
                        self.decoded_sample_rate = sr;

                        self.stretch_rx = None;
                        self.pending_stretched = None;
                        self.playing_stretched = false;

                        let source = VecSource::new(raw_samples, ch, sr);

                        let sink = sink.lock().unwrap();
                        sink.stop();
                        sink.set_speed(1.0);
                        sink.append(source);
                        sink.play();
                        self.is_playing = true;
                        self.playback_start = Some(Instant::now());
                        self.song_duration = total_duration;
                        self.seek_offset = Duration::from_secs(0);
                        drop(sink);
                        self.update_terminal_title();
                    }
                    Err(e) => {
                        eprintln!("Warning: Could not decode audio file '{}': {e}", song.name);
                    }
                },
                Err(e) => {
                    eprintln!("Warning: Could not open audio file '{}': {e}", song.name);
                }
            }
        } else {
            eprintln!("Warning: No audio sink available. Cannot play '{}'", self.songs[index].name);
        }

        if should_stretch {
            self.spawn_stretch();
        }

        Ok(())
    }

    fn play_or_pause(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.songs.is_empty() {
            return Ok(());
        }

        if self.selected_index != self.current_index || self.decoded_samples.is_none() {
            self.play_song(self.selected_index)?;
        } else if self.is_playing {
            self.pause_playback();
            self.update_terminal_title();
        } else {
            self.resume_playback();
            self.update_terminal_title();
        }
        Ok(())
    }

    fn next_song(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.songs.is_empty() {
            return Ok(());
        }

        let next_index = if self.random_mode {
            // Simple random selection using timestamp
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as usize;
            let mut indices: Vec<usize> = (0..self.songs.len()).collect();
            indices.retain(|&i| i != self.current_index);
            if indices.is_empty() {
                self.current_index
            } else {
                indices[timestamp % indices.len()]
            }
        } else if self.current_index + 1 >= self.songs.len() {
            if self.loop_mode { 0 } else { self.current_index }
        } else {
            self.current_index + 1
        };

        self.play_song(next_index)
    }

    fn previous_song(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.songs.is_empty() {
            return Ok(());
        }

        let prev_index = if self.random_mode {
            // Simple random selection using timestamp
            let timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as usize;
            let mut indices: Vec<usize> = (0..self.songs.len()).collect();
            indices.retain(|&i| i != self.current_index);
            if indices.is_empty() {
                self.current_index
            } else {
                indices[timestamp % indices.len()]
            }
        } else if self.current_index == 0 {
            if self.loop_mode { self.songs.len() - 1 } else { 0 }
        } else {
            self.current_index - 1
        };

        self.play_song(prev_index)
    }

    fn move_selection(&mut self, direction: i32) {
        if self.songs.is_empty() {
            return;
        }

        let len = self.songs.len();
        if direction > 0 {
            self.selected_index = (self.selected_index + 1) % len;
        } else if direction < 0 {
            self.selected_index = if self.selected_index == 0 { len - 1 } else { self.selected_index - 1 };
        }
        self.list_state.select(Some(self.selected_index));
    }

    fn get_playback_progress(&self) -> (Duration, Option<Duration>) {
        (self.current_song_position(), self.song_duration)
    }

    fn format_duration(duration: Duration) -> String {
        let total_seconds = duration.as_secs();
        let minutes = total_seconds / 60;
        let seconds = total_seconds % 60;
        format!("{minutes:02}:{seconds:02}")
    }

    fn pause_playback(&mut self) {
        if self.is_playing {
            // Store current song position before pausing
            self.seek_offset = self.current_song_position();

            if let Some(ref sink) = self.sink {
                let sink = sink.lock().unwrap();
                sink.pause();
            }
            self.is_playing = false;
            self.playback_start = None;
            self.update_terminal_title();
        }
    }

    fn resume_playback(&mut self) {
        if !self.is_playing && !self.songs.is_empty() {
            if let Some(stretched) = self.pending_stretched.take() {
                self.apply_stretched(stretched);
                return;
            }
            if let Some(ref sink) = self.sink {
                let sink = sink.lock().unwrap();
                sink.play();
                self.is_playing = true;
                self.playback_start = Some(Instant::now());
                self.update_terminal_title();
            }
        }
    }

    fn seek(&mut self, offset_seconds: i32) {
        if self.songs.is_empty() {
            return;
        }

        let current_position = self.current_song_position();
        let seek_duration = Duration::from_secs(offset_seconds.unsigned_abs().into());
        let new_position = if offset_seconds < 0 {
            if current_position > seek_duration {
                current_position - seek_duration
            } else {
                Duration::from_secs(0)
            }
        } else {
            current_position + seek_duration
        };

        // Determine which samples to use: stretched or original
        let samples = if let Some(ref raw) = self.decoded_samples {
            raw.clone()
        } else {
            return;
        };

        let sr = self.decoded_sample_rate;
        let ch = self.decoded_channels;

        if let Some(ref sink_arc) = self.sink {
            let source = VecSource::new(samples, ch, sr);
            let skipped = source.skip_duration(new_position);

            let sink = sink_arc.lock().unwrap();
            sink.stop();
            sink.set_speed(1.0);
            sink.append(skipped);
            sink.play();
            drop(sink);

            self.seek_offset = new_position;
            self.playback_start = Some(Instant::now());
            self.playing_stretched = false;
            self.is_playing = true;

            // If we had a non-1.0 rate, re-trigger stretch from new position
            if (self.playback_rate - 1.0).abs() >= 0.01 {
                self.spawn_stretch();
                // Pause and wait for stretch like change_playback_rate does
                if let Some(ref sink) = self.sink {
                    let sink = sink.lock().unwrap();
                    sink.pause();
                }
                self.is_playing = false;
            }
        }
    }

    fn change_playback_rate(&mut self, delta: f32) {
        let new_rate = (self.playback_rate + delta).clamp(0.25, 4.0);
        let new_rate = (new_rate * 100.0).round() / 100.0;

        // Save current song position before changing rate
        let song_pos = self.current_song_position();
        self.seek_offset = song_pos;
        self.playback_start = None;

        self.playback_rate = new_rate;
        self.playing_stretched = false;

        // Pause audio while stretch computes — avoids chipmunk/horror pitch artifacts
        if let Some(ref sink) = self.sink {
            let sink = sink.lock().unwrap();
            sink.pause();
        }
        self.is_playing = false;

        self.spawn_stretch();
    }

    fn reset_playback_rate(&mut self) {
        let song_pos = self.current_song_position();
        let was_stretched = self.playing_stretched;
        let was_stretching = self.stretch_rx.is_some();

        self.playback_rate = 1.0;
        self.stretch_rx = None;
        self.pending_stretched = None;
        self.playing_stretched = false;

        if was_stretched || was_stretching {
            // Reload original samples at current position and resume
            if let Some(raw) = self.decoded_samples.clone() {
                let sr = self.decoded_sample_rate;
                let ch = self.decoded_channels;
                if let Some(ref sink_arc) = self.sink {
                    let source = VecSource::new(raw, ch, sr);
                    let skipped = source.skip_duration(song_pos);
                    let sink = sink_arc.lock().unwrap();
                    sink.stop();
                    sink.set_speed(1.0);
                    sink.append(skipped);
                    sink.play();
                    drop(sink);
                    self.seek_offset = song_pos;
                    self.playback_start = Some(Instant::now());
                    self.is_playing = true;
                }
            }
        }
        // If not stretched and not stretching, audio is already playing original at speed 1.0
    }

    fn spawn_stretch(&mut self) {
        if let (Some(raw), Some(_sink)) = (&self.decoded_samples, &self.sink) {
            let ch = self.decoded_channels;
            let rate = self.playback_rate;

            if (rate - 1.0).abs() < 0.01 {
                self.stretch_rx = None;
                self.pending_stretched = None;
                return;
            }

            // Extract chunk from current position (60s) for fast processing
            let sr = self.decoded_sample_rate;
            let samples_per_sec = sr as usize * ch as usize;
            let song_pos = self.seek_offset;
            let start_sample = (song_pos.as_secs_f64() * samples_per_sec as f64) as usize;
            // Align to channel boundary
            let start_sample = (start_sample / ch as usize) * ch as usize;
            let chunk_samples = samples_per_sec * 60;
            let end_sample = (start_sample + chunk_samples).min(raw.len());
            let chunk = raw[start_sample..end_sample].to_vec();

            if chunk.is_empty() {
                return;
            }

            let (tx, rx) = mpsc::channel();
            self.stretch_rx = Some(rx);
            self.pending_stretched = None;

            std::thread::spawn(move || {
                let result = time_stretch_wsola(&chunk, ch, rate);
                if !result.is_empty() {
                    let _ = tx.send((result, rate));
                } else {
                    let _ = tx.send((Vec::new(), rate));
                }
            });
        }
    }

    fn check_stretch_result(&mut self) {
        if let Some(ref rx) = self.stretch_rx {
            match rx.try_recv() {
                Ok((stretched, rate)) => {
                    self.stretch_rx = None;
                    if (self.playback_rate - rate).abs() >= 0.01 {
                        return;
                    }
                    if stretched.is_empty() {
                        // Stretch failed — resume original audio at current position
                        self.resume_original_audio();
                    } else {
                        self.apply_stretched(stretched);
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.stretch_rx = None;
                    // Thread died — resume original audio
                    self.resume_original_audio();
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
    }

    /// Resume playing original (un-stretched) audio from current position.
    /// Used as fallback when stretch fails.
    fn resume_original_audio(&mut self) {
        if let Some(raw) = self.decoded_samples.clone() {
            let sr = self.decoded_sample_rate;
            let ch = self.decoded_channels;
            let song_pos = self.seek_offset; // playback_start is None when paused
            if let Some(ref sink_arc) = self.sink {
                let source = VecSource::new(raw, ch, sr);
                let skipped = source.skip_duration(song_pos);
                let sink = sink_arc.lock().unwrap();
                sink.stop();
                sink.set_speed(1.0);
                sink.append(skipped);
                sink.play();
                drop(sink);
                self.playback_start = Some(Instant::now());
                self.is_playing = true;
                self.playing_stretched = false;
            }
        }
    }

    /// Returns the current position in the original song (accounting for playback rate).
    fn current_song_position(&self) -> Duration {
        let elapsed = if let Some(start_time) = self.playback_start {
            start_time.elapsed()
        } else {
            Duration::from_secs(0)
        };

        if self.playing_stretched {
            // Stretched audio plays at sink speed 1.0, but the audio itself is
            // time-compressed. 1 second of wall-clock = playback_rate seconds of song.
            self.seek_offset + elapsed.mul_f32(self.playback_rate)
        } else {
            // Original audio plays at sink speed 1.0 (no set_speed).
            // 1 second of wall-clock = 1 second of song.
            self.seek_offset + elapsed
        }
    }

    fn apply_stretched(&mut self, stretched: Vec<f32>) {
        if stretched.is_empty() {
            return;
        }
        let sink_arc = match self.sink {
            Some(ref s) => s.clone(),
            None => return,
        };

        let sr = self.decoded_sample_rate;
        let ch = self.decoded_channels;

        // The stretched chunk starts at seek_offset (set before spawning stretch).
        // No need to skip — it's already trimmed to the right starting position.
        let source = VecSource::new(stretched, ch, sr);

        {
            let sink = sink_arc.lock().unwrap();
            sink.stop();
            sink.set_speed(1.0);
            sink.append(source);
            sink.play();
        }

        self.playing_stretched = true;
        // seek_offset stays at the song position where the chunk starts
        self.playback_start = Some(Instant::now());
        self.is_playing = true;
        self.update_terminal_title();
    }

    fn fuzzy_search(&mut self, query: &str) {
        if query.is_empty() {
            self.filtered_songs = (0..self.songs.len()).collect();
        } else {
            let query_lower = query.to_lowercase();
            let mut matches: Vec<(usize, f32)> = self
                .songs
                .iter()
                .enumerate()
                .filter_map(|(index, song)| {
                    let song_name_lower = song.name.to_lowercase();
                    let score = Self::fuzzy_match_score(&query_lower, &song_name_lower);
                    if score > 0.0 { Some((index, score)) } else { None }
                })
                .collect();

            matches.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            self.filtered_songs = matches.into_iter().map(|(index, _)| index).collect();
        }

        if !self.filtered_songs.is_empty() {
            self.selected_index = self.filtered_songs[0];
            self.list_state.select(Some(0));
        }
    }

    fn fuzzy_match_score(query: &str, text: &str) -> f32 {
        if query.is_empty() {
            return 1.0;
        }

        if text.contains(query) {
            let exact_match_bonus = if text == query { 2.0 } else { 1.5 };
            let starts_with_bonus = if text.starts_with(query) { 1.2 } else { 1.0 };
            return exact_match_bonus * starts_with_bonus;
        }

        let mut score = 0.0;
        let query_chars: Vec<char> = query.chars().collect();
        let text_chars: Vec<char> = text.chars().collect();
        let mut query_index = 0;

        for (text_index, text_char) in text_chars.iter().enumerate() {
            if query_index < query_chars.len() && *text_char == query_chars[query_index] {
                score += 1.0 / (text_index as f32 + 1.0);
                query_index += 1;
            }
        }

        if query_index == query_chars.len() {
            score / query_chars.len() as f32
        } else {
            0.0
        }
    }

    fn enter_search_mode(&mut self) {
        self.search_mode = true;
        self.search_query.clear();
        self.fuzzy_search("");
    }

    fn exit_search_mode(&mut self) {
        self.search_mode = false;
        self.search_query.clear();
        self.filtered_songs = (0..self.songs.len()).collect();
        self.list_state.select(Some(self.selected_index));
    }

    fn get_display_songs(&self) -> Vec<(usize, &Song)> {
        if self.search_mode {
            self.filtered_songs.iter().map(|&index| (index, &self.songs[index])).collect()
        } else {
            self.songs.iter().enumerate().collect()
        }
    }

    fn move_selection_in_search(&mut self, direction: i32) {
        if self.filtered_songs.is_empty() {
            return;
        }

        let current_filtered_index = self.filtered_songs.iter().position(|&index| index == self.selected_index).unwrap_or(0);

        let new_filtered_index = if direction > 0 {
            (current_filtered_index + 1) % self.filtered_songs.len()
        } else if direction < 0 {
            if current_filtered_index == 0 {
                self.filtered_songs.len() - 1
            } else {
                current_filtered_index - 1
            }
        } else {
            current_filtered_index
        };

        self.selected_index = self.filtered_songs[new_filtered_index];
        self.list_state.select(Some(new_filtered_index));
    }

    fn jump_to_first(&mut self) {
        if self.songs.is_empty() {
            return;
        }

        if self.search_mode {
            if !self.filtered_songs.is_empty() {
                self.selected_index = self.filtered_songs[0];
                self.list_state.select(Some(0));
            }
        } else {
            self.selected_index = 0;
            self.list_state.select(Some(0));
        }
    }

    fn jump_to_last(&mut self) {
        if self.songs.is_empty() {
            return;
        }

        if self.search_mode {
            if !self.filtered_songs.is_empty() {
                let last_index = self.filtered_songs.len() - 1;
                self.selected_index = self.filtered_songs[last_index];
                self.list_state.select(Some(last_index));
            }
        } else {
            self.selected_index = self.songs.len() - 1;
            self.list_state.select(Some(self.selected_index));
        }
    }
}

fn load_mp3_files(music_dir: Option<PathBuf>) -> Result<Vec<Song>, Box<dyn std::error::Error>> {
    let mut songs = Vec::new();

    if let Some(dir) = music_dir {
        if dir.exists() {
            visit_dir(&dir, &mut songs)?;
        } else {
            return Err(format!("Directory not found: {}", dir.display()).into());
        }
    } else {
        let dir = PathBuf::from(".");
        if dir.exists() {
            visit_dir(&dir, &mut songs)?;
        }
    }

    songs.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(songs)
}

fn visit_dir(dir: &PathBuf, songs: &mut Vec<Song>) -> Result<(), Box<dyn std::error::Error>> {
    if dir.is_dir() {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.is_dir() {
                visit_dir(&path, songs)?;
            } else if let Some(extension) = path.extension() {
                if extension.to_str().unwrap_or("").to_lowercase() == "mp3" {
                    let name = path.file_stem().and_then(|s| s.to_str()).unwrap_or("Unknown").to_string();

                    songs.push(Song { name, path: path.clone() });
                }
            }
        }
    }
    Ok(())
}

fn ui(f: &mut Frame, player: &Player) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Title
            Constraint::Min(8),    // Song list
            Constraint::Length(3), // Progress bar
            Constraint::Length(3), // Status
        ])
        .split(f.area());

    // Title
    let title = Paragraph::new("MUSIX")
        .style(Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD))
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(PRIMARY_COLOR)));
    f.render_widget(title, chunks[0]);

    // Song list
    let display_songs = player.get_display_songs();
    let items: Vec<ListItem> = display_songs
        .iter()
        .enumerate()
        .map(|(_display_index, &(actual_index, song))| {
            let playing_indicator = if actual_index == player.current_index && player.is_playing {
                "♪ "
            } else {
                "  "
            };

            let content = format!("{playing_indicator}{}. {}", actual_index + 1, song.name);

            let style = if actual_index == player.current_index && player.is_playing {
                Style::default().fg(HIGHLIGHT_COLOR).add_modifier(Modifier::BOLD)
            } else if actual_index == player.selected_index {
                Style::default().fg(PRIMARY_COLOR)
            } else {
                Style::default().fg(Color::White)
            };

            ListItem::new(content).style(style)
        })
        .collect();

    let songs_title = if player.search_mode {
        format!("Songs - Search: {}", player.search_query)
    } else {
        "Songs".to_string()
    };

    let songs_list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(songs_title)
                .border_style(Style::default().fg(PRIMARY_COLOR)),
        )
        .highlight_style(Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD))
        .scroll_padding(1);

    f.render_stateful_widget(songs_list, chunks[1], &mut player.list_state.clone());

    // Progress bar
    let (elapsed, total) = player.get_playback_progress();
    let progress_ratio = if let Some(duration) = total {
        if duration.as_secs() > 0 {
            (elapsed.as_secs() as f64 / duration.as_secs() as f64).min(1.0)
        } else {
            0.0
        }
    } else {
        0.0
    };

    let progress_label_text = if let Some(duration) = total {
        format!(" {}/{} ", Player::format_duration(elapsed), Player::format_duration(duration))
    } else {
        format!(" {} ", Player::format_duration(elapsed))
    };

    let progress_bar_style = Style::default().fg(PRIMARY_COLOR).bg(Color::default());
    let progress_label = Span::styled(progress_label_text, progress_bar_style);

    let progress_bar = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Progress")
                .border_style(Style::default().fg(PRIMARY_COLOR)),
        )
        .gauge_style(progress_bar_style)
        .ratio(progress_ratio)
        .label(progress_label);
    f.render_widget(progress_bar, chunks[2]);

    // Status
    let mode_text = if player.random_mode { "RANDOM" } else { "NORMAL" };
    let song_count = if player.search_mode {
        format!("{}/{}", player.filtered_songs.len(), player.songs.len())
    } else {
        player.songs.len().to_string()
    };

    let rate_text = if player.playback_rate == 1.0 && player.stretch_rx.is_none() {
        String::new()
    } else if player.stretch_rx.is_some() {
        format!(" | Speed: {:.2}x (processing...)", player.playback_rate)
    } else {
        format!(" | Speed: {:.2}x", player.playback_rate)
    };

    let status_content = if player.search_mode {
        vec![Line::from(vec![
            Span::raw(format!("  Search Mode | Songs: {}{} | ", song_count, rate_text)),
            Span::styled("Esc", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
            Span::raw(": Exit Search | "),
            Span::styled("Enter", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
            Span::raw(": Play  "),
        ])]
    } else {
        vec![Line::from(vec![
            Span::raw(format!("  Mode: {} | Songs: {}{} | ", mode_text, song_count, rate_text)),
            Span::styled("/", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
            Span::raw(": Search | "),
            Span::styled("?", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
            Span::raw(": Help  "),
        ])]
    };

    let status = Paragraph::new(status_content).alignment(Alignment::Left).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Status")
            .border_style(Style::default().fg(PRIMARY_COLOR)),
    );
    f.render_widget(status, chunks[3]);

    // Controls popup
    if player.show_controls_popup {
        let popup_area = centered_rect(60, 60, f.area());
        f.render_widget(ratatui::widgets::Clear, popup_area);

        let controls_popup = Paragraph::new(vec![
            Line::from(""),
            Line::from(vec![Span::styled("CONTROLS", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD))]).alignment(Alignment::Center),
            Line::from(""),
            Line::from(vec![
                Span::styled(" ↑/↓ or j/k", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Navigate songs"),
            ]),
            Line::from(vec![
                Span::styled(" Space/↵   ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Play/Pause"),
            ]),
            Line::from(vec![
                Span::styled(" ←/→ or h/l", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Play prev/next song"),
            ]),
            Line::from(vec![
                Span::styled(" gg/G      ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Jump to first/last"),
            ]),
            Line::from(vec![
                Span::styled(" /         ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Enter search mode"),
            ]),
            Line::from(vec![
                Span::styled(" n/N       ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Next/prev search"),
            ]),
            Line::from(vec![
                Span::styled(" ,/.       ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Seek ±5 seconds"),
            ]),
            Line::from(vec![
                Span::styled(" R         ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Toggle random mode"),
            ]),
            Line::from(vec![
                Span::styled(" +/=       ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Increase speed (+0.1x)"),
            ]),
            Line::from(vec![
                Span::styled(" -         ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Decrease speed (-0.1x)"),
            ]),
            Line::from(vec![
                Span::styled(" 0         ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Reset speed (1.0x)"),
            ]),
            Line::from(vec![
                Span::styled(" q/Esc     ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Exit application"),
            ]),
            Line::from(vec![
                Span::styled(" ?         ", Style::default().fg(PRIMARY_COLOR).add_modifier(Modifier::BOLD)),
                Span::raw(" - Toggle this help"),
            ]),
        ])
        .alignment(Alignment::Left)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Help")
                .border_style(Style::default().fg(PRIMARY_COLOR)),
        );
        f.render_widget(controls_popup, popup_area);
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: ratatui::prelude::Rect) -> ratatui::prelude::Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn run_player(music_dir: Option<PathBuf>) -> Result<(), Box<dyn std::error::Error>> {
    let auto_play = music_dir.is_some();
    let mut player = match Player::new(music_dir) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("Player initialization failed: {e}");
            eprintln!("Error details: {e:?}");
            std::process::exit(1);
        }
    };

    if player.songs.is_empty() {
        println!("No MP3 files found in the current directory.");
        println!();
        println!("Usage: musix [folder]");
        println!("  musix              - play MP3s from current directory");
        println!("  musix <folder>     - play MP3s from specified folder");
        return Ok(());
    }

    if auto_play {
        let _ = player.play_song(0);
    }

    match enable_raw_mode() {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Failed to enable raw mode: {e}");
            return Err(e.into());
        }
    }

    let mut stdout = io::stdout();
    match execute!(stdout, EnterAlternateScreen) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("Failed to enter alternate screen: {e}");
            return Err(e.into());
        }
    }

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = match Terminal::new(backend) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("Failed to create terminal: {e}");
            return Err(e.into());
        }
    };

    let result = main_loop(&mut terminal, &mut player);

    // Clean shutdown of audio to prevent warning messages
    if let Some(ref sink) = player.sink {
        let sink = sink.lock().unwrap();
        sink.stop();
    }

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    // Reset terminal title
    let _ = execute!(io::stdout(), SetTitle("Terminal"));

    result
}

fn main_loop(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, player: &mut Player) -> Result<(), Box<dyn std::error::Error>> {
    loop {
        terminal.draw(|f| ui(f, player))?;

        if let Ok(true) = event::poll(Duration::from_millis(100)) {
            if let Ok(Event::Key(key)) = event::read() {
                // Reset g_pressed state for any key except 'g'
                if key.code != KeyCode::Char('g') || key.modifiers != KeyModifiers::NONE {
                    player.g_pressed = false;
                }

                match key {
                    KeyEvent {
                        code: KeyCode::Esc,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.show_controls_popup {
                            player.show_controls_popup = false;
                        } else if player.search_mode {
                            player.exit_search_mode();
                        } else {
                            break;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('c'),
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    } => break,

                    KeyEvent {
                        code: KeyCode::Up,
                        modifiers: KeyModifiers::NONE,
                        ..
                    }
                    | KeyEvent {
                        code: KeyCode::Char('k'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.move_selection_in_search(-1);
                        } else {
                            player.move_selection(-1);
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Down,
                        modifiers: KeyModifiers::NONE,
                        ..
                    }
                    | KeyEvent {
                        code: KeyCode::Char('j'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.move_selection_in_search(1);
                        } else {
                            player.move_selection(1);
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Enter,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        let _ = player.play_or_pause();
                        if player.search_mode {
                            player.exit_search_mode();
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char(' '),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push(' ');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            let _ = player.play_or_pause();
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Left,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if !player.search_mode {
                            player.previous_song()?;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Right,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if !player.search_mode {
                            player.next_song()?;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('h'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('h');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.previous_song()?;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('l'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('l');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.next_song()?;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('n'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.move_selection_in_search(1);
                        }
                        // In normal mode, 'n' has no special meaning, fall through to default char handler
                    }

                    KeyEvent {
                        code: KeyCode::Char('N'),
                        modifiers: KeyModifiers::SHIFT,
                        ..
                    } => {
                        if player.search_mode {
                            player.move_selection_in_search(-1);
                        }
                        // In normal mode, 'N' has no special meaning, ignore
                    }

                    KeyEvent {
                        code: KeyCode::Char('g'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('g');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            if player.g_pressed {
                                // Second 'g' - jump to first song
                                player.jump_to_first();
                                player.g_pressed = false;
                            } else {
                                // First 'g' - set flag and wait for second 'g'
                                player.g_pressed = true;
                            }
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('G'),
                        modifiers: KeyModifiers::SHIFT,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('G');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.jump_to_last();
                            player.g_pressed = false; // Reset g_pressed state
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('q'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('q');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            break; // Quit the application
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('r'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('r');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.random_mode = !player.random_mode;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('+' | '='),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push(match key.code {
                                KeyCode::Char('+') => '+',
                                _ => '=',
                            });
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.change_playback_rate(0.1);
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('-'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('-');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.change_playback_rate(-0.1);
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('0'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push('0');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.reset_playback_rate();
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('?'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if !player.search_mode {
                            player.show_controls_popup = !player.show_controls_popup;
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('<') | KeyCode::Char(','),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            let c = if key.code == KeyCode::Char('<') { '<' } else { ',' };
                            player.search_query.push(c);
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.seek(-5); // Seek backward 5 seconds
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('>') | KeyCode::Char('.'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            let c = if key.code == KeyCode::Char('>') { '>' } else { '.' };
                            player.search_query.push(c);
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        } else {
                            player.seek(5); // Seek forward 5 seconds
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char('/'),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if !player.search_mode {
                            player.enter_search_mode();
                        } else {
                            player.search_query.push('/');
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Backspace,
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.pop();
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        }
                    }

                    KeyEvent {
                        code: KeyCode::Char(c),
                        modifiers: KeyModifiers::NONE,
                        ..
                    } => {
                        if player.search_mode {
                            player.search_query.push(c);
                            let query = player.search_query.clone();
                            player.fuzzy_search(&query);
                        }
                    }

                    _ => {}
                }
            }
        }

        // Check if stretch completed in background
        player.check_stretch_result();

        // Check if current song/chunk finished
        if player.is_playing && player.stretch_rx.is_none() {
            if let Some(ref sink) = player.sink {
                let sink = sink.lock().unwrap();
                if sink.empty() {
                    drop(sink);

                    let song_pos = player.current_song_position();
                    let song_done = match player.song_duration {
                        Some(dur) => song_pos >= dur,
                        None => true,
                    };

                    if player.playing_stretched && !song_done {
                        // Stretched chunk ended but song continues — stretch next chunk
                        player.seek_offset = song_pos;
                        player.playback_start = None;
                        player.playing_stretched = false;
                        player.is_playing = false;
                        player.spawn_stretch();
                    } else {
                        // Song actually finished — play next
                        player.is_playing = false;
                        player.playback_start = None;
                        player.seek_offset = Duration::from_secs(0);
                        player.playing_stretched = false;
                        player.next_song()?;
                    }
                }
            }
        }
    }

    Ok(())
}

fn main() {
    let music_dir = std::env::args().nth(1).map(PathBuf::from);

    if let Some(ref dir) = music_dir {
        if !dir.exists() {
            eprintln!("Error: Directory not found: {}", dir.display());
            std::process::exit(1);
        }
        if !dir.is_dir() {
            eprintln!("Error: Not a directory: {}", dir.display());
            std::process::exit(1);
        }
    }

    if let Err(e) = run_player(music_dir) {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_duration() {
        assert_eq!(Player::format_duration(Duration::from_secs(0)), "00:00");
        assert_eq!(Player::format_duration(Duration::from_secs(30)), "00:30");
        assert_eq!(Player::format_duration(Duration::from_secs(60)), "01:00");
        assert_eq!(Player::format_duration(Duration::from_secs(125)), "02:05");
    }
}
