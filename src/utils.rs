pub struct HumanBytes(pub u64);

impl std::fmt::Display for HumanBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let bytes = self.0 as f64;
        let units = ["B", "KB", "MB", "GB", "TB", "PB"];
        let mut unit_idx = 0;
        let mut value = bytes;

        while value >= 1024.0 && unit_idx < units.len() - 1 {
            value /= 1024.0;
            unit_idx += 1;
        }

        if unit_idx == 0 {
            write!(f, "{} {}", value as u64, units[unit_idx])
        } else {
            write!(f, "{:.2} {}", value, units[unit_idx])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_human_bytes() {
        assert_eq!(format!("{}", HumanBytes(500)), "500 B");
        assert_eq!(format!("{}", HumanBytes(1024)), "1.00 KB");
        assert_eq!(format!("{}", HumanBytes(1024 * 1024)), "1.00 MB");
        assert_eq!(format!("{}", HumanBytes(1024 * 1024 * 1024)), "1.00 GB");
    }
}
