//! AppleSkin for Horizon: food-related HUD improvements.
//!
//! A port of squeek502's AppleSkin (Unlicense):
//! - **Saturation overlay**: your saturation as gold outlines over the food bar.
//! - **Exhaustion underlay**: how close you are to losing the next food or saturation point, as a
//!   bar behind the food bar (synced from the server when it runs this mod, as single player does).
//! - **Held food preview**: the food, saturation and (estimated) health the food in your hand
//!   would restore, flashing on the bars.
//! - **Tooltips**: a food's hunger and saturation as icons.
//!
//! The icon positions follow the game's own health and food bar, its shaking included.
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use mod_api::{
    ClientHost, GuiDraw, GuiLayerPreEvent, Hand, HealthHudEvent, HealthInputs,
    HostSide, Hooks, ItemKey, ItemPart, PayloadCodec, PayloadId, PlayerTickPostEvent,
    RegistrationContext, Registrar, RenderGuiPostEvent, SettingSpec, Tag, ToClient,
    TooltipComponentDrawEvent, TooltipComponentEvent, item::item_stack::ItemStack,
};

pub const ID: &str = "appleskin";

fn sprite(name: &str) -> String {
    format!("appleskin:gui/sprites/{name}")
}
fn vanilla(name: &str) -> String {
    format!("gui/sprites/{name}")
}

// ---------------------------------------------------------------- the game's own randomness

/// `java.util.Random` (`LegacyRandomSource`), which the bars shake by.
pub struct JavaRandom(i64);

impl JavaRandom {
    const MULT: i64 = 0x5DEE_CE66D;
    const MASK: i64 = (1 << 48) - 1;
    pub fn new(seed: i64) -> Self {
        Self((seed ^ Self::MULT) & Self::MASK)
    }
    fn next(&mut self, bits: u32) -> i32 {
        self.0 = (self.0.wrapping_mul(Self::MULT).wrapping_add(0xB)) & Self::MASK;
        (self.0 >> (48 - bits)) as i32
    }
    pub fn next_int(&mut self, bound: i32) -> i32 {
        if bound & (-bound) == bound {
            return ((i64::from(bound) * i64::from(self.next(31))) >> 31) as i32;
        }
        loop {
            let bits = self.next(31);
            let val = bits % bound;
            if bits.wrapping_sub(val).wrapping_add(bound - 1) >= 0 {
                return val;
            }
        }
    }
}

/// `Mth.ceil` on a float, as the bars use it.
fn ceil(x: f32) -> i32 {
    let i = x as i32;
    if x > i as f32 { i + 1 } else { i }
}

// ---------------------------------------------------------------- where the icons are

/// What the bars were drawn from this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Bars {
    pub tick: i32,
    pub health: f32,
    pub max_health: f32,
    pub absorption: f32,
    pub food: i32,
    pub saturation: f32,
    pub hunger: bool,
    pub hardcore: bool,
    pub regeneration: bool,
    pub vehicle_hearts: i32,
    pub width: i32,
    pub height: i32,
}

impl Bars {
    pub fn from_inputs(i: &HealthInputs, width: i32, height: i32) -> Self {
        Self {
            tick: i.tick,
            health: i.health,
            max_health: i.max_health,
            absorption: i.absorption,
            food: i.food,
            saturation: i.saturation,
            hunger: i.hunger,
            hardcore: i.hardcore,
            regeneration: i.regeneration,
            vehicle_hearts: i.vehicle_hearts,
            width,
            height,
        }
    }
}

/// The icons' places, as the game's health and food rows put them (`Hud.extractPlayerHealth`).
#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The food bar's right edge and its row.
    pub right: i32,
    pub top: i32,
    /// Each food icon's height this frame, from the right.
    pub food_y: [i32; 10],
    /// Each heart container's corner (not the absorption ones), by container index.
    pub hearts: Vec<(i32, i32)>,
    pub food_bar: bool,
}

pub fn layout(b: &Bars) -> Layout {
    let mut random = JavaRandom::new(i64::from(b.tick.wrapping_mul(312_871)));
    let (x_left, right, top) = (b.width / 2 - 91, b.width / 2 + 91, b.height - 39);
    let current = ceil(b.health);
    let max_health = b.max_health.max(current as f32);
    let absorption = ceil(b.absorption);
    let rows = ceil((max_health + absorption as f32) / 2.0 / 10.0);
    let row_height = (10 - (rows - 2)).max(3);
    let offset_index = if b.regeneration {
        b.tick % ceil(max_health + 5.0)
    } else {
        -1
    };
    let containers = (f64::from(max_health / 2.0)).ceil() as i32;
    let absorb_containers = (f64::from(absorption) / 2.0).ceil() as i32;
    let mut hearts = vec![(0, 0); containers.max(0) as usize];
    for c in (0..containers + absorb_containers).rev() {
        let (row, column) = (c / 10, c % 10);
        let x = x_left + column * 8;
        let mut y = top - row * row_height;
        if current + absorption <= 4 {
            y += random.next_int(2);
        }
        if c < containers && c == offset_index {
            y -= 2;
        }
        if c < containers {
            hearts[c as usize] = (x, y);
        }
    }
    let mut food_y = [top; 10];
    let food_bar = b.vehicle_hearts == 0;
    if food_bar {
        for y in &mut food_y {
            if b.saturation <= 0.0 && b.tick % (b.food * 3 + 1) == 0 {
                *y += random.next_int(3) - 1;
            }
        }
    }
    Layout {
        right,
        top,
        food_y,
        hearts,
        food_bar,
    }
}

/// A food icon's left edge, `i` from the right.
pub fn food_x(right: i32, i: i32) -> i32 {
    right - i * 8 - 9
}

/// AppleSkin's flashing: its alpha climbs and falls by 0.125 a tick between -0.5 and 1.5, and
/// shows clamped to 0..1, times the maximum.
pub fn flash_alpha(tick: i32, max: f32) -> f32 {
    let t = tick.rem_euclid(32) as f32;
    let unclamped = if t <= 16.0 {
        -0.5 + 0.125 * t
    } else {
        1.5 - 0.125 * (t - 16.0)
    };
    unclamped.clamp(0.0, 1.0) * max
}

/// Which saturation sprite shows `effective` saturation points on one icon.
pub fn saturation_frame(effective: f32) -> Option<usize> {
    if effective >= 1.0 {
        Some(3)
    } else if effective > 0.5 {
        Some(2)
    } else if effective > 0.25 {
        Some(1)
    } else if effective > 0.0 {
        Some(0)
    } else {
        None
    }
}

/// The saturation icons `drawSaturationOverlay` draws: `(icon index, frame)`.
pub fn saturation_icons(gained: f32, saturation: f32) -> Vec<(i32, usize)> {
    let modified = (saturation + gained).clamp(0.0, 20.0);
    let start = if gained != 0.0 {
        ((saturation / 2.0) as i32).max(0)
    } else {
        0
    };
    let end = (modified / 2.0).ceil() as i32;
    (start..end)
        .filter_map(|i| saturation_frame(modified / 2.0 - i as f32).map(|f| (i, f)))
        .collect()
}

/// The food icons `drawHungerOverlay` draws: `(icon index, full)`, a half icon when not full.
pub fn hunger_icons(restored: i32, food: i32) -> Vec<(i32, bool)> {
    let modified = (food + restored).clamp(0, 20);
    let start = (food / 2).max(0);
    let end = (modified as f32 / 2.0).ceil() as i32;
    (start..end)
        .filter_map(|i| {
            let idx = i * 2 + 1;
            if idx < modified {
                Some((i, true))
            } else if idx == modified {
                Some((i, false))
            } else {
                None
            }
        })
        .collect()
}

/// The health eating `food` would bring back over time, as AppleSkin estimates it: the game's
/// natural regeneration run on the new food, saturation and exhaustion.
pub fn estimated_health(
    health: f32,
    max_health: f32,
    food: i32,
    saturation: f32,
    exhaustion: f32,
    nutrition: i32,
    food_saturation: f32,
) -> f32 {
    let mut food = (food + nutrition).min(20);
    let mut saturation = (saturation + food_saturation).min(food as f32);
    let mut exhaustion = exhaustion;
    let mut gained = 0.0;
    for _ in 0..1000 {
        if health + gained >= max_health {
            break;
        }
        if saturation > 0.0 && food >= 20 {
            let spent = saturation.min(6.0);
            gained += spent / 6.0;
            exhaustion += spent;
        } else if food >= 18 {
            gained += 1.0;
            exhaustion += 6.0;
        } else {
            break;
        }
        while exhaustion >= 4.0 {
            exhaustion -= 4.0;
            if saturation > 0.0 {
                saturation = (saturation - 1.0).max(0.0);
            } else {
                food = (food - 1).max(0);
            }
        }
    }
    gained.min(max_health - health).max(0.0)
}

// ---------------------------------------------------------------- food values

/// A food's hunger and saturation (`minecraft:food`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Food {
    pub nutrition: i32,
    pub saturation: f32,
    pub always_eat: bool,
}

pub fn food_from_tag(tag: &Tag) -> Option<Food> {
    let number = |key: &str| -> Option<f32> {
        match tag.get(key)? {
            Tag::Int(v) => Some(*v as f32),
            Tag::Short(v) => Some(*v as f32),
            Tag::Byte(v) => Some(*v as f32),
            Tag::Float(bits) => Some(f32::from_bits(*bits)),
            Tag::Double(bits) => Some(f64::from_bits(*bits) as f32),
            _ => None,
        }
    };
    Some(Food {
        nutrition: number("nutrition")? as i32,
        saturation: number("saturation").unwrap_or(0.0),
        always_eat: matches!(tag.get("can_always_eat"), Some(Tag::Byte(1))),
    })
}

pub fn food_of(host: &dyn ClientHost, stack: &ItemStack) -> Option<Food> {
    let patched = stack.components().and_then(|c| {
        c.as_nbt()
            .iter()
            .find(|(k, _)| k.to_string_lossy() == "minecraft:food")
            .map(|(_, v)| v.clone())
    });
    let tag = patched.or_else(|| host.item_default_component(stack.item(), "minecraft:food"))?;
    food_from_tag(&tag)
}

/// The vanilla foods whose tooltips show their values.
pub const FOODS: &[&str] = &[
    "minecraft:apple", "minecraft:golden_apple", "minecraft:enchanted_golden_apple",
    "minecraft:melon_slice", "minecraft:sweet_berries", "minecraft:glow_berries",
    "minecraft:chorus_fruit", "minecraft:carrot", "minecraft:golden_carrot", "minecraft:potato",
    "minecraft:baked_potato", "minecraft:poisonous_potato", "minecraft:beetroot",
    "minecraft:beef", "minecraft:cooked_beef", "minecraft:porkchop", "minecraft:cooked_porkchop",
    "minecraft:mutton", "minecraft:cooked_mutton", "minecraft:chicken", "minecraft:cooked_chicken",
    "minecraft:rabbit", "minecraft:cooked_rabbit", "minecraft:cod", "minecraft:cooked_cod",
    "minecraft:salmon", "minecraft:cooked_salmon", "minecraft:tropical_fish",
    "minecraft:pufferfish", "minecraft:bread", "minecraft:cookie", "minecraft:pumpkin_pie",
    "minecraft:rotten_flesh", "minecraft:spider_eye", "minecraft:mushroom_stew",
    "minecraft:beetroot_soup", "minecraft:rabbit_stew", "minecraft:suspicious_stew",
    "minecraft:dried_kelp", "minecraft:honey_bottle",
];

/// The tooltip's rows: food icons (`(full, half)` counts) and saturation icons (frames).
pub fn tooltip_icons(food: Food) -> (i32, Vec<usize>) {
    let hunger = ceil(food.nutrition.max(0) as f32 / 2.0);
    let sat = food.saturation.max(0.0) / 2.0;
    let count = sat.ceil() as i32;
    let frames = (0..count)
        .filter_map(|i| saturation_frame(sat - i as f32))
        .collect();
    (hunger, frames)
}

pub const TOOLTIP_MAX_ICONS: i32 = 10;

// ---------------------------------------------------------------- settings

pub fn settings() -> Vec<SettingSpec> {
    let t = |key: &str, default: bool| {
        SettingSpec::toggle(key, format!("appleskin.setting.{key}"), default)
            .tooltip(format!("appleskin.setting.{key}.tooltip"))
    };
    vec![
        t("saturation_overlay", true),
        t("exhaustion_underlay", true),
        t("food_values_overlay", true),
        t("food_values_offhand", true),
        t("health_overlay", true),
        t("tooltip", true),
        SettingSpec::range(
            "max_flash_alpha",
            "appleskin.setting.max_flash_alpha",
            0.0,
            1.0,
            0.05,
            0.65,
        )
        .tooltip("appleskin.setting.max_flash_alpha.tooltip"),
    ]
}

// ---------------------------------------------------------------- the mod

/// The saturation and exhaustion a server with this mod sends its player.
#[derive(Clone, Copy, Debug, PartialEq, PayloadCodec)]
pub struct FoodSync {
    pub saturation: f32,
    pub exhaustion: f32,
}

static SYNC: OnceLock<ToClient<FoodSync>> = OnceLock::new();
static SYNCED: Mutex<Option<FoodSync>> = Mutex::new(None);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Default)]
struct AppleSkin {
    bars: Mutex<Option<Bars>>,
    /// FIX: la capa de vida/comida ya paso este frame y falta dibujar la saturacion encima.
    pending: Mutex<bool>,
    /// What each server player was last sent.
    sent: Mutex<HashMap<u128, FoodSync>>,
}

impl AppleSkin {
    fn on(host: &dyn ClientHost, key: &str) -> bool {
        host.setting_bool(key)
            .or_else(|| host.setting_bool(&format!("{ID}:{key}")))
            .unwrap_or(true)
    }

    /// The food in hand to preview, if the player could eat it now.
    fn held_food(host: &dyn ClientHost, food_level: i32) -> Option<Food> {
        let player = host.local_player()?;
        let living = host.living(player.entity())?;
        let mut hands = vec![Hand::Main];
        if Self::on(host, "food_values_offhand") {
            hands.push(Hand::Off);
        }
        hands.into_iter().find_map(|hand| {
            let stack = living.hand_item(host, hand)?;
            let food = food_of(host, &stack)?;
            (food_level < 20 || food.always_eat).then_some(food)
        })
    }

    fn saturation_and_exhaustion(host: &dyn ClientHost, bars: &Bars) -> (f32, f32) {
        if let Some(sync) = *lock(&SYNCED) {
            return (sync.saturation, sync.exhaustion);
        }
        let food = host.local_player().and_then(|p| p.food(host));
        (
            food.map_or(bars.saturation, |f| f.saturation),
            food.map_or(0.0, |f| f.exhaustion),
        )
    }
}

fn draw_sprite(draw: &mut dyn mod_api::DrawContext, name: &str, x: i32, y: i32, alpha: f32) {
    let a = ((alpha.clamp(0.0, 1.0) * 255.0) as u32) << 24;
    draw.sprite(
        name,
        [0.0, 0.0, 9.0, 9.0],
        [x as f32, y as f32, 9.0, 9.0],
        Some(a | 0x00FF_FFFF),
    );
}

#[mod_api::horizon_mod]
impl Hooks for AppleSkin {
    fn register(&self, r: &mut dyn Registrar, _: &RegistrationContext) {
        for setting in settings() {
            let _ = r.register_setting(setting);
        }
        for id in FOODS {
            let key = r.item_key(id);
            r.item_part(key, Tooltip(key));
        }
        let mut payloads = r.payload_handlers();
        let view = payloads.registrar("1");
        let token = view.optional().play_to_client_typed::<FoodSync>(
            PayloadId("appleskin:food_sync".into()),
            |value, _| {
                *lock(&SYNCED) = Some(value);
                Ok(())
            },
        );
        let _ = SYNC.set(token);
    }

    // The server tells its players their saturation and exhaustion, which vanilla doesn't send.
    fn player_tick_post(&self, side: HostSide, event: &mut PlayerTickPostEvent) {
        let HostSide::Server(host) = side else {
            return;
        };
        let Some(food) = event.player.food(&*host) else {
            return;
        };
        let now = FoodSync {
            saturation: food.saturation,
            exhaustion: food.exhaustion,
        };
        let id = event.player.uuid();
        let changed = lock(&self.sent).get(&id).is_none_or(|last| {
            (last.saturation - now.saturation).abs() >= 0.01
                || (last.exhaustion - now.exhaustion).abs() >= 0.01
        });
        if changed && let Some(sync) = SYNC.get() {
            if sync.send(host, event.player, &now).is_ok() {
                lock(&self.sent).insert(id, now);
            }
        }
    }

    // The bars' inputs; the game draws its own bars.
    fn health_hud(&self, _: &mut dyn ClientHost, event: &mut HealthHudEvent<'_>) -> Option<Vec<GuiDraw>> {
        *lock(&self.bars) = Some(Bars::from_inputs(event.inputs, event.wi, event.hi));
        None
    }

    // The exhaustion bar goes behind the food bar.
    fn gui_layer_pre(&self, host: &dyn ClientHost, event: &mut GuiLayerPreEvent<'_>) -> mod_api::Flow {
        // FIX: la saturacion se dibujaba en render_gui_post, que en esta Nightly queda debajo de la
        // barra de comida (o no se pinta). Ahora se dibuja al empezar la capa SIGUIENTE a la de
        // vida/comida, o sea justo encima de los muslos.
        if event.layer != "minecraft:player_health" {
            let mut pending = lock(&self.pending);
            if *pending {
                *pending = false;
                drop(pending);
                if let Some(bars) = *lock(&self.bars) {
                    self.draw_overlays(host, &bars, &mut *event.draw);
                }
            }
            return mod_api::Flow::Continue;
        }
        *lock(&self.pending) = true;
        if !Self::on(host, "exhaustion_underlay") {
            return mod_api::Flow::Continue;
        }
        let Some(bars) = *lock(&self.bars) else {
            return mod_api::Flow::Continue;
        };
        if bars.vehicle_hearts != 0 {
            return mod_api::Flow::Continue;
        }
        let (_, exhaustion) = Self::saturation_and_exhaustion(host, &bars);
        let ratio = (exhaustion / 4.0).clamp(0.0, 1.0);
        let width = (ratio * 81.0) as i32;
        if width > 0 {
            let right = bars.width / 2 + 91;
            let top = bars.height - 39;
            event.draw.sprite(
                &sprite("exhaustion"),
                [(81 - width) as f32, 0.0, width as f32, 9.0],
                [(right - width) as f32, top as f32, width as f32, 9.0],
                Some(0xBFFF_FFFF),
            );
        }
        mod_api::Flow::Continue
    }

    fn render_gui_post(&self, host: &dyn ClientHost, event: &mut RenderGuiPostEvent<'_>) {
        // FIX: respaldo si ninguna capa vino despues de la de vida/comida.
        {
            let mut pending = lock(&self.pending);
            if !*pending {
                return;
            }
            *pending = false;
        }
        let Some(bars) = *lock(&self.bars) else {
            return;
        };
        self.draw_overlays(host, &bars, &mut *event.draw);
    }
}

impl AppleSkin {
    /// Saturacion, vista previa de la comida en la mano y vida estimada.
    fn draw_overlays(&self, host: &dyn ClientHost, bars: &Bars, draw: &mut dyn mod_api::DrawContext) {
        let bars = *bars;
        let place = layout(&bars);
        let (saturation, exhaustion) = Self::saturation_and_exhaustion(host, &bars);
        let sat_sprite = |frame: usize| {
            if bars.hunger {
                sprite(&format!("saturation_hunger_{frame}"))
            } else {
                sprite(&format!("saturation_{frame}"))
            }
        };
        if place.food_bar && Self::on(host, "saturation_overlay") {
            for (i, frame) in saturation_icons(0.0, saturation) {
                let x = food_x(place.right, i);
                draw_sprite(draw, &sat_sprite(frame), x, place.food_y[i as usize], 1.0);
            }
        }
        if !Self::on(host, "food_values_overlay") {
            return;
        }
        let Some(food) = Self::held_food(host, bars.food) else {
            return;
        };
        let max_alpha = host.setting_f64("max_flash_alpha").unwrap_or(0.65) as f32;
        let alpha = flash_alpha(bars.tick, max_alpha);
        if place.food_bar {
            let (full, half, empty) = if bars.hunger {
                ("hud/food_full_hunger", "hud/food_half_hunger", "hud/food_empty_hunger")
            } else {
                ("hud/food_full", "hud/food_half", "hud/food_empty")
            };
            for (i, is_full) in hunger_icons(food.nutrition, bars.food) {
                let (x, y) = (food_x(place.right, i), place.food_y[i as usize]);
                draw_sprite(draw, &vanilla(empty), x, y, alpha * 0.25);
                draw_sprite(draw, &vanilla(if is_full { full } else { half }), x, y, alpha);
            }
            if Self::on(host, "saturation_overlay") {
                let new_food = (bars.food + food.nutrition).min(20) as f32;
                let gained = (saturation + food.saturation).min(new_food) - saturation;
                if gained > 0.0 {
                    for (i, frame) in saturation_icons(gained, saturation) {
                        let x = food_x(place.right, i);
                        draw_sprite(draw, &sat_sprite(frame), x, place.food_y[i as usize], alpha);
                    }
                }
            }
        }
        if Self::on(host, "health_overlay") {
            let gain = estimated_health(
                bars.health,
                bars.max_health,
                bars.food,
                saturation,
                exhaustion,
                food.nutrition,
                food.saturation,
            );
            let current = ceil(bars.health);
            let target = ceil(bars.health + gain).min(ceil(bars.max_health));
            let prefix = if bars.hardcore { "hud/heart/hardcore_" } else { "hud/heart/" };
            for (c, &(x, y)) in place.hearts.iter().enumerate() {
                let halves = c as i32 * 2;
                if halves + 2 <= current || halves >= target {
                    continue;
                }
                let half = halves + 1 == target;
                let name = format!("{prefix}{}", if half { "half" } else { "full" });
                draw_sprite(draw, &vanilla(&name), x, y, alpha);
            }
        }
    }
}

/// The food values in a food's tooltip.
struct Tooltip(ItemKey);

impl Tooltip {
    fn food(&self, host: &dyn ClientHost, stack: &ItemStack) -> Option<Food> {
        if !host.setting_bool("tooltip").unwrap_or(true) {
            return None;
        }
        let food = food_of(host, stack)?;
        (food.nutrition > 0 || food.saturation > 0.0).then_some(food)
    }
}

fn row_width(icons: i32) -> f32 {
    if icons > TOOLTIP_MAX_ICONS {
        9.0 + 30.0
    } else {
        icons as f32 * 9.0
    }
}

#[mod_api::horizon_part]
impl ItemPart for Tooltip {
    fn tooltip_component(&self, host: &dyn ClientHost, event: &mut TooltipComponentEvent<'_>) -> Option<[f32; 2]> {
        let food = self.food(host, event.stack)?;
        let (hunger, frames) = tooltip_icons(food);
        let width = row_width(hunger).max(row_width(frames.len() as i32));
        let height = if frames.is_empty() { 11.0 } else { 21.0 };
        Some([width.max(9.0), height])
    }

    fn tooltip_component_draw(&self, host: &dyn ClientHost, event: &mut TooltipComponentDrawEvent<'_>) {
        let _ = self.0;
        let Some(food) = self.food(host, event.stack) else {
            return;
        };
        let (hunger, frames) = tooltip_icons(food);
        let [x, y] = event.origin;
        let (x, y) = (x as i32, y as i32 + 1);
        let draw = &mut *event.draw;
        // Hunger: the game's food icons, from the left.
        let shown = hunger.min(TOOLTIP_MAX_ICONS);
        for i in 0..shown {
            let half = i == hunger - 1 && food.nutrition % 2 == 1 && hunger <= TOOLTIP_MAX_ICONS;
            let ix = x + i * 9;
            draw_sprite(draw, &vanilla("hud/food_empty"), ix, y, 1.0);
            let name = if half { "hud/food_half" } else { "hud/food_full" };
            draw_sprite(draw, &vanilla(name), ix, y, 1.0);
        }
        if hunger > TOOLTIP_MAX_ICONS {
            let text = format!("x{}", f64::from(food.nutrition) / 2.0);
            draw.text(&text, [(x + 11) as f32, (y + 1) as f32], 0xFFAA_AAAA, true);
        }
        // Saturation: AppleSkin's own icons.
        let y = y + 10;
        let count = frames.len() as i32;
        if count > TOOLTIP_MAX_ICONS {
            draw_sprite(draw, &sprite("tooltip_saturation_3"), x, y, 1.0);
            let text = format!("x{:.1}", food.saturation / 2.0);
            draw.text(&text, [(x + 11) as f32, (y + 1) as f32], 0xFFAA_AAAA, true);
        } else {
            for (i, frame) in frames.into_iter().enumerate() {
                draw_sprite(draw, &sprite(&format!("tooltip_saturation_{frame}")), x + i as i32 * 9, y, 1.0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn java_random_matches_java() {
        // new Random(42).nextInt(10) twice: 0, then 3.
        let mut r = JavaRandom::new(42);
        assert_eq!(r.next_int(10), 0);
        assert_eq!(r.next_int(10), 3);
        // new Random(0).nextInt(2): 1 (a power of two takes the other path).
        assert_eq!(JavaRandom::new(0).next_int(2), 1);
    }

    #[test]
    fn every_setting_is_one_the_game_takes() {
        for s in settings() {
            s.check().unwrap();
        }
    }

    fn bars() -> Bars {
        Bars {
            tick: 7,
            health: 20.0,
            max_health: 20.0,
            absorption: 0.0,
            food: 14,
            saturation: 3.0,
            hunger: false,
            hardcore: false,
            regeneration: false,
            vehicle_hearts: 0,
            width: 320,
            height: 240,
        }
    }

    #[test]
    fn the_food_bar_stands_still_with_saturation() {
        let l = layout(&bars());
        assert_eq!(l.right, 251);
        assert_eq!(l.food_y, [201; 10]);
        assert_eq!(l.hearts.len(), 10);
        assert_eq!(l.hearts[0], (69, 201));
        assert_eq!(l.hearts[9], (141, 201));
    }

    #[test]
    fn the_food_bar_shakes_without_saturation() {
        let mut b = bars();
        b.saturation = 0.0;
        b.food = 2;
        b.tick = 7 * 7; // 49 % 7 == 0
        let l = layout(&b);
        assert!(l.food_y.iter().all(|y| (200..=202).contains(y)));
    }

    #[test]
    fn saturation_icons_fill_from_the_right() {
        assert_eq!(saturation_icons(0.0, 3.0), [(0, 3), (1, 1)]);
        assert_eq!(saturation_icons(0.0, 0.0), []);
        assert_eq!(saturation_icons(2.0, 3.0), [(1, 3), (2, 1)]);
    }

    #[test]
    fn hunger_preview_covers_the_new_food() {
        assert_eq!(hunger_icons(4, 14), [(7, true), (8, true)]);
        assert_eq!(hunger_icons(3, 14), [(7, true), (8, false)]);
        assert_eq!(hunger_icons(10, 18), [(9, true)]);
    }

    #[test]
    fn flashing_stays_within_its_maximum() {
        let alphas: Vec<f32> = (0..64).map(|t| flash_alpha(t, 0.65)).collect();
        assert!(alphas.iter().all(|a| (0.0..=0.65).contains(a)));
        assert_eq!(flash_alpha(0, 0.65), 0.0);
        assert!((flash_alpha(12, 0.65) - 0.65).abs() < 1e-6);
    }

    #[test]
    fn health_is_estimated_from_natural_regeneration() {
        // Full food and saturation heals; low food does not.
        assert!(estimated_health(10.0, 20.0, 18, 0.0, 0.0, 8, 12.8) > 0.0);
        assert_eq!(estimated_health(10.0, 20.0, 4, 0.0, 0.0, 2, 1.2), 0.0);
        assert_eq!(estimated_health(20.0, 20.0, 18, 5.0, 0.0, 8, 12.8), 0.0);
    }

    #[test]
    fn food_values_read_the_component() {
        let tag = Tag::compound(vec![
            ("nutrition", Tag::Int(8)),
            ("saturation", Tag::Float(12.8f32.to_bits())),
        ]);
        let food = food_from_tag(&tag).unwrap();
        assert_eq!(food.nutrition, 8);
        assert!((food.saturation - 12.8).abs() < 1e-6);
        let (hunger, frames) = tooltip_icons(food);
        assert_eq!(hunger, 4);
        assert_eq!(frames.len(), 7);
    }
}
