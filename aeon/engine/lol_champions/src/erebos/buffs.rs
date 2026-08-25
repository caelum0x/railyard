use bevy::prelude::*;
use lol_core::base::buff::Buff;

/// 蚀刻印记 (Eclipse Mark)
///
/// Erebos 的技能命中敌人时挂载的印记，供未来的连招/引爆逻辑读取。
/// 与 Ahri 的 `BuffCharm` 同样是"携带计时的标记型 buff"。
#[derive(Component, Debug, Clone)]
#[require(Buff = Buff { name: "EclipseMark" })]
pub struct BuffEclipseMark {
    pub timer: Timer,
}

impl BuffEclipseMark {
    pub fn new(duration: f32) -> Self {
        Self {
            timer: Timer::from_seconds(duration, TimerMode::Once),
        }
    }
}

/// 暮纱 (Umbral Veil) — W 激活期间的自身状态标记
///
/// 只作为"W 是否处于激活期"的可查询状态；实际的移速/治疗由通用 buff
/// (`BuffMoveSpeed` / `BuffSelfHeal`) 提供，本标记不重复施加数值效果。
#[derive(Component, Debug, Clone)]
#[require(Buff = Buff { name: "UmbralVeil" })]
pub struct BuffUmbralVeil {
    pub timer: Timer,
}

impl BuffUmbralVeil {
    pub fn new(duration: f32) -> Self {
        Self {
            timer: Timer::from_seconds(duration, TimerMode::Once),
        }
    }
}
