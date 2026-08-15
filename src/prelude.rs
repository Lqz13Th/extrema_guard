//! Stable convenience imports for composing guard applications.

pub use crate::arch::{
    executor::{
        GuardExecutor, GuardExecutorConfig, PositionSnapshot, RunMode, is_own_id,
        load_guard_executor_config, position_key,
    },
    okx_outside_range_cancel_module::{
        base::OkxOutsideRangeCancel,
        utils::{OkxOutsideRangeCancelConfig, load_okx_outside_range_cancel_config},
    },
    profit_lock_module::{
        base::ProfitLock,
        utils::{ProfitLockConfig, ProfitLockRule, load_profit_lock_config},
    },
    safety::SafetyLimits,
    static_protect_module::{
        base::StaticProtect,
        utils::{StaticProtectConfig, StaticRule, load_static_protect_config},
    },
};
