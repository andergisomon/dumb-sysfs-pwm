use dumb_sysfs_pwm::*;

const PWM_CHIP: u32 = 0;

fn main() -> Result<()> {

    let mut pwm_a = PwmBuilder::new(PWM_CHIP, 1, 20_000).build().unwrap();

    pwm_a.set_enable(true)?;
    pwm_a.set_duty_cycle(0.5)?;

    Ok(())
}
