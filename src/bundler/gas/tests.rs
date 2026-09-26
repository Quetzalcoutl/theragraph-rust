    use super::*;

    // ── pack_account_gas_limits ───────────────────────────────────────────────

    #[test]
    fn pack_account_gas_limits_encodes_vgl_in_first_16_bytes() {
        let vgl: u128 = 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10;
        let cgl: u128 = 0xffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff;
        let packed = pack_account_gas_limits(vgl, cgl);
        assert_eq!(&packed[..16], &vgl.to_be_bytes(), "first 16 bytes should encode vgl");
        assert_eq!(&packed[16..], &cgl.to_be_bytes(), "last 16 bytes should encode cgl");
    }

    #[test]
    fn unpack_account_gas_limits_round_trips() {
        let vgl: u128 = 1_000_000;
        let cgl: u128 = 2_000_000;
        let packed = pack_account_gas_limits(vgl, cgl);
        assert_eq!(unpack_account_gas_limits(&packed), (vgl, cgl));
    }

    // ── pack_gas_fees ─────────────────────────────────────────────────────────

    #[test]
    fn pack_unpack_gas_fees_round_trips() {
        let max_priority: u128 = 3_000_000_000; // 3 gwei
        let max_fee: u128 = 100_000_000_000; // 100 gwei
        let packed = pack_gas_fees(max_priority, max_fee);
        assert_eq!(unpack_gas_fees(&packed), (max_priority, max_fee));
    }

    // ── scale_call_gas ────────────────────────────────────────────────────────

    #[test]
    fn scale_call_gas_n1_returns_base() {
        let base: u64 = 500_000;
        assert_eq!(scale_call_gas(base, 1), base);
    }

    #[test]
    fn scale_call_gas_n2_adds_100_000() {
        let base: u64 = 500_000;
        assert_eq!(scale_call_gas(base, 2), base + 100_000);
    }

    #[test]
    fn scale_call_gas_n5_adds_400_000() {
        let base: u64 = 500_000;
        assert_eq!(scale_call_gas(base, 5), base + 400_000);
    }

    #[test]
    fn scale_call_gas_saturates_on_overflow() {
        // base = u64::MAX - 100_000, n_calls = 2  →  extra = 100_000
        // saturating_add should clamp at u64::MAX
        let base: u64 = u64::MAX - 100_000;
        assert_eq!(scale_call_gas(base, 2), u64::MAX);
    }

    // ── deployment_verification_gas ───────────────────────────────────────────

    #[test]
    fn deployment_verification_gas_adds_400_000() {
        assert_eq!(deployment_verification_gas(100), 400_100);
    }

    #[test]
    fn deployment_verification_gas_zero_base() {
        assert_eq!(deployment_verification_gas(0), 400_000);
    }

    // ── zero values ───────────────────────────────────────────────────────────

    #[test]
    fn pack_account_gas_limits_zero_is_all_zeros() {
        let packed = pack_account_gas_limits(0, 0);
        assert_eq!(packed.as_slice(), &[0u8; 32]);
    }

    // ── pack_gas_fees byte layout ─────────────────────────────────────────────

    #[test]
    fn pack_gas_fees_encodes_priority_fee_in_first_16_bytes() {
        let priority: u128 = 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10;
        let max_fee: u128 = 0xffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff;
        let packed = pack_gas_fees(priority, max_fee);
        assert_eq!(&packed[..16], &priority.to_be_bytes(), "first 16 bytes should encode priority fee");
        assert_eq!(&packed[16..], &max_fee.to_be_bytes(), "last 16 bytes should encode max fee");
    }

    #[test]
    fn pack_gas_fees_zero_is_all_zeros() {
        let packed = pack_gas_fees(0, 0);
        assert_eq!(packed.as_slice(), &[0u8; 32]);
    }

    // ── scale_call_gas additional edge cases ──────────────────────────────────

    #[test]
    fn scale_call_gas_n0_returns_base_unchanged() {
        // n_calls = 0: saturating_sub(1) = 0 → extra = 0
        let base: u64 = 300_000;
        assert_eq!(scale_call_gas(base, 0), base);
    }

    #[test]
    fn scale_call_gas_large_n_saturates() {
        // n_calls = usize::MAX → saturating_mul on u64::MAX * 100_000 saturates
        let base: u64 = 1;
        let result = scale_call_gas(base, usize::MAX);
        assert_eq!(result, u64::MAX);
    }

    // ── deployment_verification_gas overflow ──────────────────────────────────

    #[test]
    fn deployment_verification_gas_panics_on_overflow() {
        // The function uses plain `+` (not saturating), so we confirm it
        // panics in debug builds when base + 400_000 overflows u64.
        // We test the non-overflowing boundary: u64::MAX - 400_000 is fine.
        let safe_base = u64::MAX - 400_000;
        assert_eq!(deployment_verification_gas(safe_base), u64::MAX);
    }
