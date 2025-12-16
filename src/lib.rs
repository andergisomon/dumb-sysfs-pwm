// Copyright 2016, Paul Osborne <osbpau@gmail.com>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/license/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// Copyright 2016, Paul Osborne <osbpau@gmail.com>
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/license/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option.  This file may not be copied, modified, or distributed
// except according to those terms.
//
// Portions of this implementation are based on work by Nat Pryce:
// https://github.com/npryce/rusty-pi/blob/master/src/pi/gpio.rs

//! PWM access under Linux using the PWM sysfs interface

use std::fs::{self, File, OpenOptions};
use std::io::{prelude::*, SeekFrom};
use std::os::unix::io::AsRawFd;
use std::thread;
use std::time::Duration;

mod error;
pub use error::Error;

pub type Result<T> = std::result::Result<T, error::Error>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Polarity {
    Normal,
    Inverse,
}

/// Configuration for PWM initialization
#[derive(Debug, Clone)]
pub struct PwmConfig {
    pub chip: u32,
    pub channel: u32,
    pub period_ns: u32,
    pub polarity: Polarity,
}

impl PwmConfig {
    ///make sure period_ns is greater than duty_cycle in ns set later! Avoid initializing the period_ns here to 0
    pub fn new(chip: u32, channel: u32, period_ns: u32) -> Self {
        Self {
            chip,
            channel,
            period_ns,
            polarity: Polarity::Normal,
        }
    }

    pub fn with_polarity(mut self, polarity: Polarity) -> Self {
        self.polarity = polarity;
        self
    }
}

/// PWM controller with persistent file handles for real-time use.
///
/// File handles are opened once during initialization and reused
/// for all subsequent operations. Period is cached since it doesn't
/// change after setup.
pub struct Pwm {
    chip: u32,
    channel: u32,
    period_ns: u32,
    enable_file: File,
    duty_cycle_file: File,
    period_file: File,
    polarity_file: File,
    // Buffer for writing values - avoids allocation in hot path
    write_buf: [u8; 16],
    enable: bool,
    polarity: Polarity,
}

impl Pwm {
    /// Create and initialize a new PWM channel.
    ///
    /// This will:
    /// 1. Export the PWM channel if not already exported
    /// 2. Set the period and polarity
    /// 3. Open persistent file handles for enable and duty_cycle
    /// 4. Initialize duty cycle to 0 and disabled state
    pub fn new(config: PwmConfig) -> Result<Self> {
        let chip = config.chip;
        let channel = config.channel;
        let base_path = format!("/sys/class/pwm/pwmchip{}/pwm{}", chip, channel);

        // Export if needed
        Self::export(chip, channel)?;

        // Set period first (must be done before duty_cycle)
        Self::write_sysfs_file(&format!("{}/period", base_path), config.period_ns)?;

        // Set polarity (must be done while disabled)
        let polarity = config.polarity;
        let polarity_str = match config.polarity {
            Polarity::Normal => "normal",
            Polarity::Inverse => "inversed",
        };
        Self::write_sysfs_file_str(&format!("{}/polarity", base_path), polarity_str)?;

        // Set initial duty cycle to 0
        Self::write_sysfs_file(&format!("{}/duty_cycle", base_path), 0u32)?;

        // Now open persistent handles
        let enable_file = OpenOptions::new()
            .write(true)
            .open(format!("{}/enable", base_path))?;

        let duty_cycle_file = OpenOptions::new()
            .write(true)
            .open(format!("{}/duty_cycle", base_path))?;

        let period_file = OpenOptions::new()
            .write(true)
            .open(format!("{}/period", base_path))?;

        let polarity_file = OpenOptions::new()
            .write(true)
            .open(format!("{}/polarity", base_path))?;

        let mut pwm = Self {
            chip,
            channel,
            period_ns: config.period_ns,
            enable_file,
            duty_cycle_file,
            period_file,
            polarity_file,
            write_buf: [0u8; 16],
            enable: false,
            polarity
        };

        // Ensure disabled state
        pwm.set_enable(false)?;

        Ok(pwm)
    }

    /// Export the PWM channel via sysfs
    fn export(chip: u32, channel: u32) -> Result<()> {
        let pwm_path = format!("/sys/class/pwm/pwmchip{}/pwm{}", chip, channel);

        if fs::metadata(&pwm_path).is_ok() {
            // Already exported
            return Ok(());
        }

        let export_path = format!("/sys/class/pwm/pwmchip{}/export", chip);
        let mut export_file = File::create(&export_path)?;
        write!(export_file, "{}", channel)?;
        export_file.flush()?;
        export_file.sync_all()?;

        // Wait for sysfs to create the directory
        let mut retries = 50;
        while fs::metadata(&pwm_path).is_err() && retries > 0 {
            thread::sleep(Duration::from_millis(10));
            retries -= 1;
        }

        if fs::metadata(&pwm_path).is_err() {
            return Err(Error::Unexpected(format!(
                "PWM channel {} failed to export after 500ms",
                channel
            )));
        }

        // Additional delay for sysfs files to be fully ready
        thread::sleep(Duration::from_millis(10));

        Ok(())
    }

    /// Helper for one-shot sysfs writes during initialization
    fn write_sysfs_file<T: std::fmt::Display>(path: &str, value: T) -> Result<()> {
        let mut file = OpenOptions::new().write(true).open(path)?;
        write!(file, "{}", value)?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    }

    /// Helper for one-shot sysfs string writes during initialization
    fn write_sysfs_file_str(path: &str, value: &str) -> Result<()> {
        let mut file = OpenOptions::new().write(true).open(path)?;
        file.write_all(value.as_bytes())?;
        file.flush()?;
        file.sync_all()?;
        Ok(())
    }

    #[inline]
    pub fn set_enable(&mut self, enable: bool) -> Result<()> {
        let byte = if enable { b'1' } else { b'0' };

        // Seek to beginning and write
        self.enable_file.seek(SeekFrom::Start(0))?;
        self.enable_file.write_all(&[byte])?;
        self.enable_file.flush()?;

        Ok(())
    }

    #[inline]
    pub fn get_enable(&self) -> bool {
        self.enable
    }

    /// Set the duty cycle in nanoseconds.
    #[inline]
    pub fn set_duty_cycle_ns(&mut self, duty_cycle_ns: u32) -> Result<()> {
        let len = self.format_u32(duty_cycle_ns);
        self.duty_cycle_file.seek(SeekFrom::Start(0))?;
        self.duty_cycle_file.write_all(&self.write_buf[..len])?;
        self.duty_cycle_file.flush()?;
        Ok(())
    }

    /// Set the duty cycle as a ratio (0.0 to 1.0). Panics if the duty cycle is not from 0.0 to 1.0.
    /// Uses the cached period value to avoid file I/O.
    #[inline]
    pub fn set_duty_cycle(&mut self, duty_cycle: f32) -> Result<()> {
        assert!(
            (0.0..=1.0).contains(&duty_cycle),
            "Duty cycle must be between 0.0 and 1.0"
        );

        let duty_ns = (self.period_ns as f32 * duty_cycle).round() as u32;
        self.set_duty_cycle_ns(duty_ns)
    }

    /// Get the cached period in nanoseconds.
    #[inline]
    pub fn period_ns(&self) -> u32 {
        self.period_ns
    }

    #[inline]
    pub fn set_period_ns(&mut self, period_ns: u32) -> Result<()> {
        let len = self.format_u32(period_ns);
        self.period_file.seek(SeekFrom::Start(0))?;
        self.period_file.write_all(&self.write_buf[..len])?;
        self.period_file.flush()?;
        Ok(())
    }

    #[inline]
    pub fn set_polarity(&mut self, polarity: Polarity) -> Result<()> {
        self.polarity = polarity;

        let len = match polarity {
            Polarity::Inverse => {
                let pol = b"inversed";
                self.fill_write_buf_str(pol);
                pol.len()
            },
            Polarity::Normal => {
                let pol = b"normal";
                self.fill_write_buf_str(pol);
                pol.len()
            }
        };

        self.polarity_file.seek(SeekFrom::Start(0))?;
        self.polarity_file.write_all(&self.write_buf[..len])?;
        self.polarity_file.flush()?;
        Ok(())
    }

    /// Get the cached polarity
    #[inline]
    pub fn get_polarity(&self) -> Polarity {
        self.polarity
    }

    /// Get the chip number.
    #[inline]
    pub fn chip(&self) -> u32 {
        self.chip
    }

    /// Get the channel number.
    #[inline]
    pub fn channel(&self) -> u32 {
        self.channel
    }

    /// Format a u32 into the write buffer, returning the length.
    ///
    /// This avoids allocation in the hot path by using a pre-allocated buffer.
    #[inline]
    fn format_u32(&mut self, mut value: u32) -> usize {
        if value == 0 {
            self.write_buf[0] = b'0';
            return 1;
        }

        let mut pos = 0;
        let mut temp = [0u8; 16];

        while value > 0 {
            temp[pos] = b'0' + (value % 10) as u8;
            value /= 10;
            pos += 1;
        }

        // Reverse into write_buf
        for i in 0..pos {
            self.write_buf[i] = temp[pos - 1 - i];
        }

        pos
    }

    ///For convenience: writes &str into the preallocated write buffer
    #[inline]
    fn fill_write_buf_str(&mut self, value: &[u8]) -> usize {

        let pos = value.len();
        // Reverse into write_buf
        for i in 0..pos {
            self.write_buf[i] = value[i];
        }

        pos
    }

    /// Unexport the PWM channel.
    ///
    /// Called automatically on drop, but can be called manually if needed.
    pub fn unexport(&mut self) -> Result<()> {
        // Disable first
        let _ = self.set_enable(false);
        let _ = self.set_duty_cycle_ns(0);

        let pwm_path = format!("/sys/class/pwm/pwmchip{}/pwm{}", self.chip, self.channel);

        if fs::metadata(&pwm_path).is_ok() {
            let unexport_path = format!("/sys/class/pwm/pwmchip{}/unexport", self.chip);
            let mut unexport_file = File::create(&unexport_path)?;
            write!(unexport_file, "{}", self.channel)?;
            unexport_file.flush()?;
            unexport_file.sync_all()?;
        }

        Ok(())
    }

    /// Sync all pending writes to hardware.
    ///
    /// Call this if you need to ensure writes have been committed
    /// before proceeding (e.g., before reading back state).
    pub fn sync(&mut self) -> Result<()> {
        self.enable_file.sync_all()?;
        self.duty_cycle_file.sync_all()?;
        Ok(())
    }

    /// Get the raw file descriptor for the duty cycle file.
    ///
    /// Useful for advanced use cases like epoll or custom I/O.
    pub fn duty_cycle_fd(&self) -> i32 {
        self.duty_cycle_file.as_raw_fd()
    }

    /// Get the raw file descriptor for the enable file.
    pub fn enable_fd(&self) -> i32 {
        self.enable_file.as_raw_fd()
    }
}

// Safety: File handles are safe to send between threads
unsafe impl Send for Pwm {}

/// Builder for more complex PWM setups
pub struct PwmBuilder {
    config: PwmConfig,
    initial_duty_cycle: Option<f32>,
    enable: bool,
}

impl PwmBuilder {
    ///make sure period_ns will be greater than duty cycle in ns set later
    pub fn new(chip: u32, channel: u32, period_ns: u32) -> Self {
        Self {
            config: PwmConfig::new(chip, channel, period_ns),
            initial_duty_cycle: None,
            enable: false,
        }
    }

    pub fn polarity(mut self, polarity: Polarity) -> Self {
        self.config.polarity = polarity;
        self
    }

    pub fn initial_duty_cycle(mut self, duty_cycle: f32) -> Self {
        self.initial_duty_cycle = Some(duty_cycle);
        self
    }

    pub fn build(self) -> Result<Pwm> {
        let mut pwm = Pwm::new(self.config)?;

        if let Some(duty_cycle) = self.initial_duty_cycle {
            pwm.set_duty_cycle(duty_cycle)?;
        }

        if self.enable {
            pwm.set_enable(true)?;
        }

        Ok(pwm)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_fill_write_buf_str() {
        let mut buf = [0u8; 16];

        let format = |buf: &mut [u8; 16], value: &[u8]| -> usize {
            let pos = value.len();
            // Reverse into write_buf
            for i in 0..pos {
                buf[i] = value[i];
            }
            pos
        };

        let len = format(&mut buf, b"normal");
        assert_eq!(&buf[..len], b"normal");

        let len = format(&mut buf, b"inversed");
        assert_eq!(&buf[..len], b"inversed");

    }

    #[test]
    fn test_format_u32() {
        let mut buf = [0u8; 16];

        let format = |buf: &mut [u8; 16], mut value: u32| -> usize {
            if value == 0 {
                buf[0] = b'0';
                return 1;
            }
            let mut pos = 0;
            let mut temp = [0u8; 16];
            while value > 0 {
                temp[pos] = b'0' + (value % 10) as u8;
                value /= 10;
                pos += 1;
            }
            for i in 0..pos {
                buf[i] = temp[pos - 1 - i];
            }
            pos
        };

        let len = format(&mut buf, 0);
        assert_eq!(&buf[..len], b"0");

        let len = format(&mut buf, 12345);
        assert_eq!(&buf[..len], b"12345");

        let len = format(&mut buf, 20000);
        assert_eq!(&buf[..len], b"20000");
    }
}
