use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;
use serde::Deserialize;

use crate::database::repositories::diarization::MeetingSpeaker;

#[derive(Deserialize)]
pub struct ColorChoice {
    pub value: String,
}

pub static PALETTE: Lazy<Vec<ColorChoice>> = Lazy::new(|| {
    serde_json::from_str(include_str!("../../../src/lib/speaker-colors.json"))
        .expect("The bundled speaker palette must be valid")
});

/// Live labelling's default colours: speaker, overlap and needs-review.
/// Older builds stored them as CSS names. None of these is a colour a speaker
/// was given, so they're replaced with a distinct palette colour.
pub const LIVE_SPEAKER_COLOR: &str = "#3b82f6";
pub const LIVE_OVERLAP_COLOR: &str = "#f97316";
pub const LIVE_REVIEW_COLOR: &str = "#6b7280";
const LIVE_DEFAULT_COLORS: [&str; 6] = [
    LIVE_SPEAKER_COLOR,
    LIVE_OVERLAP_COLOR,
    LIVE_REVIEW_COLOR,
    "blue",
    "orange",
    "gray",
];

/// A stored colour worth keeping: a `#rrggbb` value that isn't a live default.
fn is_assigned_color(color: &str) -> bool {
    color.len() == 7
        && color.starts_with('#')
        && color[1..].chars().all(|c| c.is_ascii_hexdigit())
        && !LIVE_DEFAULT_COLORS.contains(&color)
}

fn next_color(used: &HashSet<String>) -> String {
    if let Some(color) = PALETTE.iter().find(|color| !used.contains(&color.value)) {
        return color.value.clone();
    }
    // Larger meetings still need distinct, dark text colors after the palette runs out.
    for n in 0..2_097_152u32 {
        let color = format!(
            "#{:02x}{:02x}{:02x}",
            (n >> 14) & 127,
            (n >> 7) & 127,
            n & 127
        );
        if !used.contains(&color) {
            return color;
        }
    }
    "#000000".into()
}

pub fn apply_colors(speakers: &mut [MeetingSpeaker], choices: &HashMap<String, String>) {
    // Ignore live defaults and anything that isn't a hex colour.
    let mut reserved: HashSet<String> = speakers
        .iter()
        .filter_map(|speaker| {
            speaker
                .speaker_color
                .as_ref()
                .filter(|color| is_assigned_color(color))
                .cloned()
        })
        .collect();
    let mut used = HashSet::new();
    for speaker in speakers.iter_mut() {
        let color = speaker
            .speaker_color
            .as_ref()
            .filter(|color| is_assigned_color(color) && !used.contains(*color))
            .cloned()
            .unwrap_or_else(|| next_color(&reserved));
        reserved.insert(color.clone());
        used.insert(color.clone());
        speaker.speaker_color = Some(color);
    }
    // Apply choices after defaults, so changing one speaker leaves the other colors alone.
    let mut used: HashSet<String> = choices.values().cloned().collect();
    for speaker in speakers {
        if let Some(color) = choices.get(&speaker.speaker_id) {
            speaker.speaker_color = Some(color.clone());
        } else {
            let color = speaker.speaker_color.as_ref().unwrap();
            if used.contains(color) {
                speaker.speaker_color = Some(next_color(&used));
            }
            used.insert(speaker.speaker_color.clone().unwrap());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn speakers(count: usize) -> Vec<MeetingSpeaker> {
        (0..count)
            .map(|i| MeetingSpeaker {
                speaker_id: format!("s{i}"),
                speaker_label: format!("Speaker {i}"),
                speaker_color: Some("#3b82f6".into()),
                segment_count: 1,
                verified_segment_count: 0,
            })
            .collect()
    }

    #[test]
    fn replaces_legacy_blue_with_distinct_colors_even_for_large_meetings() {
        let mut rows = speakers(30);
        apply_colors(&mut rows, &HashMap::new());
        let colors: HashSet<_> = rows
            .iter()
            .map(|row| row.speaker_color.as_ref().unwrap())
            .collect();
        assert_eq!(colors.len(), rows.len());
    }

    #[test]
    fn live_default_colors_and_names_get_palette_colors() {
        let mut rows = speakers(4);
        for (row, color) in rows.iter_mut().zip(["blue", "gray", LIVE_OVERLAP_COLOR, LIVE_REVIEW_COLOR]) {
            row.speaker_color = Some(color.into());
        }
        apply_colors(&mut rows, &HashMap::new());
        for row in &rows {
            let color = row.speaker_color.as_deref().unwrap();
            assert!(PALETTE.iter().any(|choice| choice.value == color), "{color} is not a palette colour");
        }
    }

    #[test]
    fn changing_one_color_keeps_other_defaults() {
        let mut before = speakers(8);
        apply_colors(&mut before, &HashMap::new());
        let mut after = speakers(8);
        apply_colors(
            &mut after,
            &HashMap::from([("s0".into(), PALETTE[12].value.clone())]),
        );
        assert_eq!(
            after[0].speaker_color.as_deref(),
            Some(PALETTE[12].value.as_str())
        );
        for i in 1..8 {
            assert_eq!(before[i].speaker_color, after[i].speaker_color);
        }
    }

    #[test]
    fn palette_has_readable_text_contrast_on_white() {
        for color in PALETTE.iter() {
            let rgb = u32::from_str_radix(&color.value[1..], 16).unwrap();
            let linear = |shift: u32| {
                let value = ((rgb >> shift) & 255u32) as f64 / 255.0;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            let luminance = 0.2126 * linear(16) + 0.7152 * linear(8) + 0.0722 * linear(0);
            assert!(
                1.05 / (luminance + 0.05) >= 4.5,
                "{} has low contrast",
                color.value
            );
        }
    }
}
