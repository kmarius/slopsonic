//! Bounded spectrum analysis.

use rustfft::{Fft, FftPlanner, num_complex::Complex};
use std::collections::VecDeque;
use std::sync::Arc;

const SPECTRUM_FFT_SIZE: usize = 2_048;

pub struct SpectrumFft {
    samples: VecDeque<f32>,
    input: Vec<Complex<f32>>,
    window: Vec<f32>,
    fft: Arc<dyn Fft<f32>>,
}

impl SpectrumFft {
    pub fn new() -> Self {
        let size = SPECTRUM_FFT_SIZE;
        let mut planner = FftPlanner::<f32>::new();
        let fft = planner.plan_fft_forward(size);
        let window = (0..size)
            .map(|index| {
                let position = index as f32 / (size - 1) as f32;
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * position).cos()
            })
            .collect();
        Self {
            samples: VecDeque::with_capacity(size),
            input: vec![Complex::new(0.0, 0.0); size],
            window,
            fft,
        }
    }

    pub fn push_channel_samples(&mut self, samples: &[f32], channels: usize, channel: usize) {
        for frame in samples.chunks_exact(channels) {
            if self.samples.len() == self.input.len() {
                self.samples.pop_front();
            }
            self.samples.push_back(frame[channel]);
        }
    }

    /// One-sided power at each FFT frequency, normalized by the window's coherent gain.
    pub fn spectrum_power(&mut self) -> Option<Vec<f64>> {
        if self.samples.len() < self.input.len() {
            return None;
        }
        for ((slot, sample), window) in self
            .input
            .iter_mut()
            .zip(self.samples.iter().copied())
            .zip(self.window.iter().copied())
        {
            *slot = Complex::new(sample * window, 0.0);
        }
        self.fft.process(&mut self.input);
        let coherent_gain = self.window.iter().map(|v| f64::from(*v)).sum::<f64>();
        Some(
            self.input[..=self.input.len() / 2]
                .iter()
                .enumerate()
                .map(|(i, value)| {
                    let factor = if i == 0 || i == self.input.len() / 2 {
                        1.0
                    } else {
                        2.0
                    };
                    f64::from(value.norm_sqr()) * (factor / coherent_gain).powi(2)
                })
                .collect(),
        )
    }
}

impl Default for SpectrumFft {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spectrum_power_preserves_tone_amplitude() {
        let mut fft = SpectrumFft::new();
        let bin = 48;
        let samples = (0..SPECTRUM_FFT_SIZE)
            .map(|index| {
                let phase =
                    2.0 * std::f32::consts::PI * bin as f32 * index as f32 / SPECTRUM_FFT_SIZE as f32;
                phase.sin() * 0.8
            })
            .collect::<Vec<_>>();
        fft.push_channel_samples(&samples, 1, 0);
        let power = fft.spectrum_power().expect("full FFT window");
        assert!((power[bin].sqrt() - 0.8).abs() < 1e-4);
    }
}
