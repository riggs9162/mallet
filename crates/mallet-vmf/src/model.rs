//! Document, entity, solid and side types.

use std::error::Error;
use std::fmt;

use glam::DVec3;
use mallet_kv::{Block, Item, Pair, ParseError};

/// Why a VMF could not be loaded.
#[derive(Debug, Clone, PartialEq)]
pub enum VmfError {
    /// The text is not valid KeyValues.
    Syntax(ParseError),
    /// The text parsed, but a block Mallet types is malformed.
    Invalid {
        /// Which block failed, such as `side 42`.
        context: String,
        /// What is wrong with it.
        message: String,
    },
}

impl fmt::Display for VmfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(error) => write!(f, "syntax error at {error}"),
            Self::Invalid { context, message } => write!(f, "{context}: {message}"),
        }
    }
}

impl Error for VmfError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Syntax(error) => Some(error),
            Self::Invalid { .. } => None,
        }
    }
}

impl From<ParseError> for VmfError {
    fn from(error: ParseError) -> Self {
        Self::Syntax(error)
    }
}

fn invalid(context: &str, message: impl Into<String>) -> VmfError {
    VmfError::Invalid {
        context: context.to_owned(),
        message: message.into(),
    }
}

/// A whole VMF document, with top-level blocks kept in file order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Vmf {
    /// Top-level items in file order.
    pub items: Vec<TopLevel>,
    /// Where entities and brushes were last found, for looking them up by id.
    pub(crate) index: crate::index::IdIndex,
}

/// One top-level item of a VMF.
#[derive(Debug, Clone, PartialEq)]
pub enum TopLevel {
    /// A stray top-level pair. Hammer never writes these, but they are preserved.
    Pair(Pair),
    /// The `world` block, holding worldspawn's keys and the world brushes.
    World(Entity),
    /// An `entity` block.
    Entity(Entity),
    /// Any other block, such as `versioninfo`, `viewsettings`, `cameras` or `cordons`.
    Other(Block),
}

/// What Hammer writes for a brand-new, empty map.
const NEW_MAP: &str = r#"versioninfo
{
	"editorversion" "400"
	"editorbuild" "8864"
	"mapversion" "0"
	"formatversion" "100"
	"prefab" "0"
}
visgroups
{
}
viewsettings
{
	"bSnapToGrid" "1"
	"bShowGrid" "1"
	"bShowLogicalGrid" "0"
	"nGridSpacing" "64"
	"bShow3DGrid" "0"
}
world
{
	"id" "1"
	"mapversion" "0"
	"classname" "worldspawn"
	"skyname" "sky_day01_01"
	"maxpropscreenwidth" "-1"
	"detailvbsp" "detail.vbsp"
	"detailmaterial" "detail/detailsprites"
}
cameras
{
	"activecamera" "-1"
}
cordons
{
	"active" "0"
}
"#;

impl Vmf {
    /// An empty map with just a world, laid out the way Hammer creates new maps.
    pub fn new_map() -> Self {
        Self::parse(NEW_MAP).expect("the built-in new map template is valid")
    }

    /// Parses VMF text.
    pub fn parse(text: &str) -> Result<Self, VmfError> {
        Self::from_items(mallet_kv::parse(text)?)
    }

    /// Builds a document from already-parsed KeyValues items.
    pub fn from_items(items: Vec<Item>) -> Result<Self, VmfError> {
        let items = items
            .into_iter()
            .map(|item| match item {
                Item::Pair(pair) => Ok(TopLevel::Pair(pair)),
                Item::Block(block) if block.name.eq_ignore_ascii_case("world") => {
                    Entity::from_block(block).map(TopLevel::World)
                }
                Item::Block(block) if block.name.eq_ignore_ascii_case("entity") => {
                    Entity::from_block(block).map(TopLevel::Entity)
                }
                Item::Block(block) => Ok(TopLevel::Other(block)),
            })
            .collect::<Result<_, _>>()?;
        Ok(Self::with_items(items))
    }

    /// A document made of these top-level items, in file order.
    pub fn with_items(items: Vec<TopLevel>) -> Self {
        Self {
            items,
            index: crate::index::IdIndex::default(),
        }
    }

    /// Converts the document back into KeyValues items.
    pub fn to_items(&self) -> Vec<Item> {
        self.items
            .iter()
            .map(|item| match item {
                TopLevel::Pair(pair) => Item::Pair(pair.clone()),
                TopLevel::World(world) => Item::Block(world.to_block("world")),
                TopLevel::Entity(entity) => Item::Block(entity.to_block("entity")),
                TopLevel::Other(block) => Item::Block(block.clone()),
            })
            .collect()
    }

    /// Writes the document as VMF text in Hammer's layout.
    pub fn to_text(&self) -> String {
        mallet_kv::write(&self.to_items())
    }

    /// Returns the `world` block, if the document has one.
    pub fn world(&self) -> Option<&Entity> {
        self.items.iter().find_map(|item| match item {
            TopLevel::World(world) => Some(world),
            _ => None,
        })
    }

    /// Iterates over every entity except the world, in file order.
    pub fn entities(&self) -> impl Iterator<Item = &Entity> {
        self.items.iter().filter_map(|item| match item {
            TopLevel::Entity(entity) => Some(entity),
            _ => None,
        })
    }

    /// Iterates over the world and every entity, in file order.
    pub fn all_entities(&self) -> impl Iterator<Item = &Entity> {
        self.items.iter().filter_map(|item| match item {
            TopLevel::World(entity) | TopLevel::Entity(entity) => Some(entity),
            _ => None,
        })
    }

    /// Returns the first top-level block with the given name, such as `versioninfo`.
    pub fn block(&self, name: &str) -> Option<&Block> {
        self.items.iter().find_map(|item| match item {
            TopLevel::Other(block) if block.name.eq_ignore_ascii_case(name) => Some(block),
            _ => None,
        })
    }
}

/// The world or a single entity: its keys, brushes and editor blocks in file order.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Entity {
    /// Keys, solids and other blocks in file order.
    pub items: Vec<EntityItem>,
}

/// One item inside an entity.
#[derive(Debug, Clone, PartialEq)]
pub enum EntityItem {
    /// A key, such as `classname` or `origin`.
    Key(Pair),
    /// A brush belonging to this entity.
    Solid(Solid),
    /// Any other block, such as `connections`, `editor` or `hidden`.
    Other(Block),
}

impl Entity {
    /// Builds an entity from a `world` or `entity` block.
    pub fn from_block(block: Block) -> Result<Self, VmfError> {
        let items = block
            .items
            .into_iter()
            .map(|item| match item {
                Item::Pair(pair) => Ok(EntityItem::Key(pair)),
                Item::Block(child) if child.name.eq_ignore_ascii_case("solid") => {
                    Solid::from_block(child).map(EntityItem::Solid)
                }
                Item::Block(child) => Ok(EntityItem::Other(child)),
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { items })
    }

    /// Converts the entity back into a block with the given name.
    pub fn to_block(&self, name: &str) -> Block {
        Block {
            name: name.to_owned(),
            items: self
                .items
                .iter()
                .map(|item| match item {
                    EntityItem::Key(pair) => Item::Pair(pair.clone()),
                    EntityItem::Solid(solid) => Item::Block(solid.to_block()),
                    EntityItem::Other(block) => Item::Block(block.clone()),
                })
                .collect(),
        }
    }

    /// Returns the value of the first key that matches, ignoring ASCII case.
    pub fn get(&self, key: &str) -> Option<&str> {
        self.items.iter().find_map(|item| match item {
            EntityItem::Key(pair) if pair.key.eq_ignore_ascii_case(key) => Some(pair.value.as_str()),
            _ => None,
        })
    }

    /// Sets a key, replacing the first existing value or adding it after the last key.
    pub fn set(&mut self, key: &str, value: impl Into<String>) {
        let value = value.into();
        let existing = self.items.iter_mut().find_map(|item| match item {
            EntityItem::Key(pair) if pair.key.eq_ignore_ascii_case(key) => Some(pair),
            _ => None,
        });
        if let Some(pair) = existing {
            pair.value = value;
            return;
        }
        let insert_at = self
            .items
            .iter()
            .rposition(|item| matches!(item, EntityItem::Key(_)))
            .map_or(0, |index| index + 1);
        self.items.insert(insert_at, EntityItem::Key(Pair::new(key, value)));
    }

    /// Removes the first key that matches, ignoring ASCII case, and returns its value.
    pub fn remove(&mut self, key: &str) -> Option<String> {
        let index = self
            .items
            .iter()
            .position(|item| matches!(item, EntityItem::Key(pair) if pair.key.eq_ignore_ascii_case(key)))?;
        match self.items.remove(index) {
            EntityItem::Key(pair) => Some(pair.value),
            _ => None,
        }
    }

    /// The entity's unique `id`, if present and numeric.
    pub fn id(&self) -> Option<u32> {
        self.get("id")?.trim().parse().ok()
    }

    /// The entity's `classname`, or an empty string when missing.
    pub fn classname(&self) -> &str {
        self.get("classname").unwrap_or("")
    }

    /// The entity's `origin`, if present and well formed.
    pub fn origin(&self) -> Option<DVec3> {
        parse_vec3(self.get("origin")?)
    }

    /// Iterates over the brushes that belong to this entity, excluding hidden ones.
    pub fn solids(&self) -> impl Iterator<Item = &Solid> {
        self.items.iter().filter_map(|item| match item {
            EntityItem::Solid(solid) => Some(solid),
            _ => None,
        })
    }

    /// The entity's `editor` block, holding its colour, visgroups and logical position.
    pub fn editor(&self) -> Option<&Block> {
        self.items.iter().find_map(|item| match item {
            EntityItem::Other(block) if block.name.eq_ignore_ascii_case("editor") => Some(block),
            _ => None,
        })
    }

    /// The colour Hammer draws this entity with in the 2D views.
    pub fn editor_color(&self) -> Option<[u8; 3]> {
        parse_color(self.editor()?.get("color")?)
    }

    /// Iterates mutably over the entity's brushes.
    pub fn solids_mut(&mut self) -> impl Iterator<Item = &mut Solid> {
        self.items.iter_mut().filter_map(|item| match item {
            EntityItem::Solid(solid) => Some(solid),
            _ => None,
        })
    }

    /// Moves the entity's brushes and origin, keeping textures in place on the brushes.
    /// An overlay's plane and corners move with it.
    pub fn translate(&mut self, offset: DVec3) {
        for solid in self.solids_mut() {
            solid.translate(offset);
        }
        if let Some(origin) = self.origin() {
            self.set("origin", format_vec3(origin + offset));
        }
        crate::overlay::transform_entity(self, &glam::DMat4::from_translation(offset));
    }

    /// The entity's outputs, from its `connections` blocks, in file order.
    pub fn connections(&self) -> Vec<Connection> {
        self.items
            .iter()
            .filter_map(connections_block)
            .flat_map(Block::pairs)
            .filter_map(Connection::from_pair)
            .collect()
    }

    /// Replaces the entity's outputs. The `connections` block keeps its place, or is
    /// added after the last key; it is removed when there are no outputs. The separator
    /// the map already uses (comma, or the escape character newer Hammers write) is kept.
    pub fn set_connections(&mut self, connections: &[Connection]) {
        let escaped = self
            .items
            .iter()
            .filter_map(connections_block)
            .flat_map(Block::pairs)
            .any(|pair| pair.value.contains(CONNECTION_ESCAPE));
        let separator = if escaped { CONNECTION_ESCAPE } else { ',' };
        let position = self.items.iter().position(|item| connections_block(item).is_some());
        self.items.retain(|item| connections_block(item).is_none());
        if connections.is_empty() {
            return;
        }
        let mut block = Block::new("connections");
        block.items = connections
            .iter()
            .map(|connection| Item::Pair(connection.to_pair(separator)))
            .collect();
        let at = position.unwrap_or_else(|| {
            self.items
                .iter()
                .rposition(|item| matches!(item, EntityItem::Key(_)))
                .map_or(0, |index| index + 1)
        });
        self.items.insert(at.min(self.items.len()), EntityItem::Other(block));
    }

    /// Gives the entity, its brushes and their faces fresh ids, as needed for a copy.
    pub fn renumber(&mut self, ids: &mut IdAllocator) {
        self.set("id", ids.object().to_string());
        for solid in self.solids_mut() {
            solid.renumber(ids);
        }
    }
}

fn connections_block(item: &EntityItem) -> Option<&Block> {
    match item {
        EntityItem::Other(block) if block.name.eq_ignore_ascii_case("connections") => Some(block),
        _ => None,
    }
}

/// The separator newer versions of Hammer write between the fields of an output, so
/// parameters may contain commas.
const CONNECTION_ESCAPE: char = '\u{1b}';

/// One output of an entity: when `output` fires, `input` is sent to the entities named
/// `target`, with `parameter`, after `delay` seconds, at most `refires` times (-1 for
/// no limit).
#[derive(Debug, Clone, PartialEq)]
pub struct Connection {
    pub output: String,
    pub target: String,
    pub input: String,
    pub parameter: String,
    pub delay: f64,
    pub refires: i32,
}

impl Connection {
    /// Reads a `connections` pair such as `"OnTrigger" "door,Open,,0,-1"`.
    pub fn from_pair(pair: &Pair) -> Option<Self> {
        let separator = if pair.value.contains(CONNECTION_ESCAPE) {
            CONNECTION_ESCAPE
        } else {
            ','
        };
        let fields: Vec<&str> = pair.value.split(separator).collect();
        let [target, input, parameter, delay, refires] = fields.as_slice() else {
            return None;
        };
        Some(Self {
            output: pair.key.clone(),
            target: (*target).to_owned(),
            input: (*input).to_owned(),
            parameter: (*parameter).to_owned(),
            delay: delay.trim().parse().unwrap_or(0.0),
            refires: refires.trim().parse().unwrap_or(-1),
        })
    }

    /// Writes the connection as a `connections` pair with the given field separator.
    pub fn to_pair(&self, separator: char) -> Pair {
        let fields = [
            self.target.clone(),
            self.input.clone(),
            self.parameter.clone(),
            self.delay.to_string(),
            self.refires.to_string(),
        ];
        Pair::new(self.output.clone(), fields.join(&separator.to_string()))
    }
}

/// Hands out ids that are unused in a map: one sequence for entities and solids, and one
/// for faces, matching how Hammer numbers them.
#[derive(Debug, Clone)]
pub struct IdAllocator {
    next_object: u32,
    next_side: u32,
}

impl IdAllocator {
    /// Starts after the highest ids used anywhere in the map, including inside hidden
    /// and grouped blocks that Mallet keeps verbatim.
    pub fn for_map(vmf: &Vmf) -> Self {
        let mut allocator = Self {
            next_object: 1,
            next_side: 1,
        };
        allocator.scan(&vmf.to_items(), "");
        allocator
    }

    fn scan(&mut self, items: &[Item], parent: &str) {
        for item in items {
            match item {
                Item::Pair(pair) if pair.key.eq_ignore_ascii_case("id") => {
                    let Ok(id) = pair.value.trim().parse::<u32>() else {
                        continue;
                    };
                    let next = if parent.eq_ignore_ascii_case("side") {
                        &mut self.next_side
                    } else {
                        &mut self.next_object
                    };
                    *next = (*next).max(id.saturating_add(1));
                }
                Item::Pair(_) => {}
                Item::Block(block) => self.scan(&block.items, &block.name),
            }
        }
    }

    /// Returns an unused entity or solid id.
    pub fn object(&mut self) -> u32 {
        let id = self.next_object;
        self.next_object += 1;
        id
    }

    /// Returns an unused face id.
    pub fn side(&mut self) -> u32 {
        let id = self.next_side;
        self.next_side += 1;
        id
    }
}

/// A brush: a convex solid bounded by the planes of its sides.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Solid {
    /// Keys, sides and other blocks in file order.
    pub items: Vec<SolidItem>,
}

/// One item inside a solid.
#[derive(Debug, Clone, PartialEq)]
pub enum SolidItem {
    /// A key, normally just `id`.
    Key(Pair),
    /// One face of the brush, boxed because sides are much larger than keys.
    Side(Box<Side>),
    /// Any other block, normally `editor`.
    Other(Block),
}

impl Solid {
    /// Builds a solid from a `solid` block.
    pub fn from_block(block: Block) -> Result<Self, VmfError> {
        let items = block
            .items
            .into_iter()
            .map(|item| match item {
                Item::Pair(pair) => Ok(SolidItem::Key(pair)),
                Item::Block(child) if child.name.eq_ignore_ascii_case("side") => {
                    Side::from_block(child).map(|side| SolidItem::Side(Box::new(side)))
                }
                Item::Block(child) => Ok(SolidItem::Other(child)),
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { items })
    }

    /// Converts the solid back into a `solid` block.
    pub fn to_block(&self) -> Block {
        Block {
            name: "solid".to_owned(),
            items: self
                .items
                .iter()
                .map(|item| match item {
                    SolidItem::Key(pair) => Item::Pair(pair.clone()),
                    SolidItem::Side(side) => Item::Block(side.to_block()),
                    SolidItem::Other(block) => Item::Block(block.clone()),
                })
                .collect(),
        }
    }

    /// The solid's unique `id`, if present and numeric.
    pub fn id(&self) -> Option<u32> {
        self.items.iter().find_map(|item| match item {
            SolidItem::Key(pair) if pair.key.eq_ignore_ascii_case("id") => pair.value.trim().parse().ok(),
            _ => None,
        })
    }

    /// Sets the solid's `id`, adding the key first if it is missing.
    pub fn set_id(&mut self, id: u32) {
        let existing = self.items.iter_mut().find_map(|item| match item {
            SolidItem::Key(pair) if pair.key.eq_ignore_ascii_case("id") => Some(pair),
            _ => None,
        });
        match existing {
            Some(pair) => pair.value = id.to_string(),
            None => self.items.insert(0, SolidItem::Key(Pair::new("id", id.to_string()))),
        }
    }

    /// Iterates mutably over the solid's faces.
    pub fn sides_mut(&mut self) -> impl Iterator<Item = &mut Side> {
        self.items.iter_mut().filter_map(|item| match item {
            SolidItem::Side(side) => Some(side.as_mut()),
            _ => None,
        })
    }

    /// Moves the brush, shifting texture offsets so textures stay put on its faces.
    pub fn translate(&mut self, offset: DVec3) {
        for side in self.sides_mut() {
            side.translate(offset);
        }
    }

    /// Moves every plane point, and each displacement's start position, through `map`.
    ///
    /// Texture axes are left alone, so textures keep their world alignment and size, as in
    /// Hammer with texture scale lock off. `map` must preserve orientation (no mirroring),
    /// or face normals would point into the brush.
    pub fn transform(&mut self, map: &dyn Fn(DVec3) -> DVec3) {
        for side in self.sides_mut() {
            side.transform(map);
        }
    }

    /// Gives the solid and its faces fresh ids, as needed for a copy.
    pub fn renumber(&mut self, ids: &mut IdAllocator) {
        self.set_id(ids.object());
        for side in self.sides_mut() {
            side.id = ids.side();
        }
    }

    /// Iterates over the solid's faces.
    pub fn sides(&self) -> impl Iterator<Item = &Side> {
        self.items.iter().filter_map(|item| match item {
            SolidItem::Side(side) => Some(side.as_ref()),
            _ => None,
        })
    }

    /// The solid's `editor` block.
    pub fn editor(&self) -> Option<&Block> {
        self.items.iter().find_map(|item| match item {
            SolidItem::Other(block) if block.name.eq_ignore_ascii_case("editor") => Some(block),
            _ => None,
        })
    }

    /// The colour Hammer draws this brush with in the 2D views.
    pub fn editor_color(&self) -> Option<[u8; 3]> {
        parse_color(self.editor()?.get("color")?)
    }
}

/// A texture projection axis, written as `[x y z offset] scale`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TexAxis {
    /// World-space direction the texture runs along.
    pub axis: DVec3,
    /// Texture shift in texels.
    pub offset: f64,
    /// World units per texel.
    pub scale: f64,
}

impl TexAxis {
    /// Parses `[x y z offset] scale`.
    pub fn parse(text: &str) -> Option<Self> {
        let [x, y, z, offset, scale] = parse_numbers(text)?.try_into().ok()?;
        Some(Self {
            axis: DVec3::new(x, y, z),
            offset,
            scale,
        })
    }
}

impl fmt::Display for TexAxis {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "[{} {} {} {}] {}",
            self.axis.x, self.axis.y, self.axis.z, self.offset, self.scale
        )
    }
}

/// One face of a brush.
///
/// The fields Hammer always writes are typed. Anything else, such as a `dispinfo` block
/// or Hammer++'s `vertices_plus`, is kept verbatim in [`Side::extra`] and written back in
/// the same position relative to the typed fields.
#[derive(Debug, Clone, PartialEq)]
pub struct Side {
    /// Unique face id.
    pub id: u32,
    /// Three points on the face plane, wound so the normal points out of the brush.
    pub plane: [DVec3; 3],
    /// Material path relative to `materials/`, without extension.
    pub material: String,
    /// Horizontal texture axis.
    pub uaxis: TexAxis,
    /// Vertical texture axis.
    pub vaxis: TexAxis,
    /// Texture rotation in degrees, as shown in the face edit sheet.
    pub rotation: Option<f64>,
    /// Luxels per world unit used by VRAD.
    pub lightmap_scale: Option<u32>,
    /// Bitmask of smoothing groups.
    pub smoothing_groups: Option<u32>,
    /// Every other key and block, in file order.
    pub extra: Vec<Item>,
    /// For each item in `extra`, how many typed fields came before it in the file. Items
    /// without an entry are written after all typed fields.
    pub extra_slots: Vec<usize>,
}

impl Side {
    /// Builds a side from a `side` block.
    pub fn from_block(block: Block) -> Result<Self, VmfError> {
        let context = match block.get("id") {
            Some(id) => format!("side {id}"),
            None => "side".to_owned(),
        };
        let mut id = None;
        let mut plane = None;
        let mut material = None;
        let mut uaxis = None;
        let mut vaxis = None;
        let mut rotation = None;
        let mut lightmap_scale = None;
        let mut smoothing_groups = None;
        let mut extra = Vec::new();
        let mut extra_slots = Vec::new();
        let mut typed = 0;

        for item in block.items {
            let Item::Pair(pair) = item else {
                extra.push(item);
                extra_slots.push(typed);
                continue;
            };
            typed += 1;
            match pair.key.to_ascii_lowercase().as_str() {
                "id" if id.is_none() => id = Some(parse_value(&context, &pair)?),
                "plane" if plane.is_none() => {
                    plane = Some(parse_plane(&pair.value).ok_or_else(|| bad_value(&context, &pair))?)
                }
                "material" if material.is_none() => material = Some(pair.value),
                "uaxis" if uaxis.is_none() => {
                    uaxis = Some(TexAxis::parse(&pair.value).ok_or_else(|| bad_value(&context, &pair))?)
                }
                "vaxis" if vaxis.is_none() => {
                    vaxis = Some(TexAxis::parse(&pair.value).ok_or_else(|| bad_value(&context, &pair))?)
                }
                "rotation" if rotation.is_none() => rotation = Some(parse_value(&context, &pair)?),
                "lightmapscale" if lightmap_scale.is_none() => lightmap_scale = Some(parse_value(&context, &pair)?),
                "smoothing_groups" if smoothing_groups.is_none() => {
                    smoothing_groups = Some(parse_value(&context, &pair)?)
                }
                _ => {
                    typed -= 1;
                    extra.push(Item::Pair(pair));
                    extra_slots.push(typed);
                }
            }
        }

        Ok(Self {
            id: id.ok_or_else(|| invalid(&context, "missing \"id\""))?,
            plane: plane.ok_or_else(|| invalid(&context, "missing \"plane\""))?,
            material: material.ok_or_else(|| invalid(&context, "missing \"material\""))?,
            uaxis: uaxis.ok_or_else(|| invalid(&context, "missing \"uaxis\""))?,
            vaxis: vaxis.ok_or_else(|| invalid(&context, "missing \"vaxis\""))?,
            rotation,
            lightmap_scale,
            smoothing_groups,
            extra,
            extra_slots,
        })
    }

    /// Converts the side back into a `side` block: typed fields in Hammer's order, with
    /// every extra item put back where it was among them.
    pub fn to_block(&self) -> Block {
        let typed: Vec<Pair> = [
            Some(Pair::new("id", self.id.to_string())),
            Some(Pair::new("plane", format_plane(&self.plane))),
            Some(Pair::new("material", self.material.clone())),
            Some(Pair::new("uaxis", self.uaxis.to_string())),
            Some(Pair::new("vaxis", self.vaxis.to_string())),
            self.rotation
                .map(|rotation| Pair::new("rotation", rotation.to_string())),
            self.lightmap_scale
                .map(|scale| Pair::new("lightmapscale", scale.to_string())),
            self.smoothing_groups
                .map(|groups| Pair::new("smoothing_groups", groups.to_string())),
        ]
        .into_iter()
        .flatten()
        .collect();
        let slot = |index: usize| self.extra_slots.get(index).copied().unwrap_or(usize::MAX);
        let extras_where = |keep: &dyn Fn(usize) -> bool| {
            self.extra
                .iter()
                .enumerate()
                .filter(move |(index, _)| keep(slot(*index)))
                .map(|(_, item)| Item::clone(item))
                .collect::<Vec<_>>()
        };
        let count = typed.len();
        let mut block = Block::new("side");
        for (position, pair) in typed.into_iter().enumerate() {
            block.items.extend(extras_where(&|slot| slot == position));
            block.items.push(Item::Pair(pair));
        }
        block.items.extend(extras_where(&|slot| slot >= count));
        block
    }

    /// The face's displacement data, if it is a displacement.
    pub fn dispinfo(&self) -> Option<&Block> {
        self.extra.iter().find_map(|item| match item {
            Item::Block(block) if block.name.eq_ignore_ascii_case("dispinfo") => Some(block),
            _ => None,
        })
    }

    /// Moves the face with texture lock: each axis offset changes by the distance moved
    /// along that axis in texels, so the texture stays fixed to the face. A displacement's
    /// start position moves with it.
    pub fn translate(&mut self, offset: DVec3) {
        for axis in [&mut self.uaxis, &mut self.vaxis] {
            if axis.scale != 0.0 {
                axis.offset -= offset.dot(axis.axis) / axis.scale;
            }
        }
        self.transform(&|point| point + offset);
    }

    /// Moves the plane points and a displacement's start position through `map`, leaving
    /// texture axes alone.
    pub fn transform(&mut self, map: &dyn Fn(DVec3) -> DVec3) {
        for point in &mut self.plane {
            *point = map(*point);
        }
        let dispinfo = self.extra.iter_mut().find_map(|item| match item {
            Item::Block(block) if block.name.eq_ignore_ascii_case("dispinfo") => Some(block),
            _ => None,
        });
        let Some(dispinfo) = dispinfo else {
            return;
        };
        for item in &mut dispinfo.items {
            if let Item::Pair(pair) = item {
                if pair.key.eq_ignore_ascii_case("startposition") {
                    if let Some(start) = parse_vec3(&pair.value) {
                        let moved = map(start);
                        pair.value = format!("[{} {} {}]", moved.x, moved.y, moved.z);
                    }
                }
            }
        }
    }
}

/// Formats a point as `x y z`, the way entity origins are written.
pub fn format_vec3(point: DVec3) -> String {
    format!("{} {} {}", point.x, point.y, point.z)
}

fn bad_value(context: &str, pair: &Pair) -> VmfError {
    invalid(context, format!("bad \"{}\" value \"{}\"", pair.key, pair.value))
}

fn parse_value<T: std::str::FromStr>(context: &str, pair: &Pair) -> Result<T, VmfError> {
    pair.value.trim().parse().map_err(|_| bad_value(context, pair))
}

/// Splits text such as `(1 2 3) (4 5 6)` or `[1 0 0 0] 0.25` into its numbers.
///
/// Returns `None` if any token is not a number.
pub fn parse_numbers(text: &str) -> Option<Vec<f64>> {
    text.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '[' | ']'))
        .filter(|token| !token.is_empty())
        .map(|token| token.parse().ok())
        .collect()
}

/// Parses three whitespace-separated numbers, with or without parentheses.
pub fn parse_vec3(text: &str) -> Option<DVec3> {
    let [x, y, z] = parse_numbers(text)?.try_into().ok()?;
    Some(DVec3::new(x, y, z))
}

/// Parses an `R G B` colour with components from 0 to 255.
pub fn parse_color(text: &str) -> Option<[u8; 3]> {
    let mut parts = text.split_whitespace().map(|part| part.parse::<u8>().ok());
    let color = [parts.next()??, parts.next()??, parts.next()??];
    Some(color)
}

/// Parses a side's `(x y z) (x y z) (x y z)` plane.
pub fn parse_plane(text: &str) -> Option<[DVec3; 3]> {
    let numbers = parse_numbers(text)?;
    let [ax, ay, az, bx, by, bz, cx, cy, cz] = numbers.try_into().ok()?;
    Some([DVec3::new(ax, ay, az), DVec3::new(bx, by, bz), DVec3::new(cx, cy, cz)])
}

/// Formats a plane the way Hammer writes it.
pub fn format_plane(plane: &[DVec3; 3]) -> String {
    let [a, b, c] = plane;
    format!(
        "({} {} {}) ({} {} {}) ({} {} {})",
        a.x, a.y, a.z, b.x, b.y, b.z, c.x, c.y, c.z
    )
}
