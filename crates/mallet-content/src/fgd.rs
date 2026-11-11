//! Forge Game Data (`.fgd`) files: the entity classes Hammer offers, with their keyvalues,
//! flags, inputs, outputs and editor helpers such as `size()` and `studio()`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// What kind of entity a class describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClassKind {
    /// `@BaseClass`: only inherited by other classes.
    Base,
    /// A point entity, such as `@PointClass`, `@NPCClass` or `@KeyFrameClass`.
    Point,
    /// A brush entity, `@SolidClass`.
    Solid,
}

/// One choice of a `choices` keyvalue or one bit of `spawnflags`.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    /// The value written to the map, such as `0` or `models/foo.mdl`.
    pub value: String,
    /// The name shown to the user.
    pub name: String,
    /// For flags, whether the bit is set by default.
    pub default_on: bool,
}

/// One keyvalue a class has.
#[derive(Debug, Clone, PartialEq)]
pub struct Property {
    /// The key written to the map, such as `targetname`.
    pub name: String,
    /// The FGD type in lower case, such as `string`, `choices`, `flags` or `color255`.
    pub kind: String,
    /// The name shown to the user; the key itself when the FGD gives none.
    pub display: String,
    /// The default value, possibly empty.
    pub default: String,
    /// Help text.
    pub description: String,
    /// The choices of a `choices` keyvalue or the bits of a `flags` one.
    pub choices: Vec<Choice>,
    /// Whether the FGD marks it `readonly`.
    pub read_only: bool,
}

/// An input or output of a class.
#[derive(Debug, Clone, PartialEq)]
pub struct Port {
    pub name: String,
    /// Parameter type in lower case, such as `void`, `string` or `integer`.
    pub kind: String,
    pub description: String,
}

/// One entity class, with everything it inherits from its base classes merged in.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityClass {
    pub name: String,
    pub kind: ClassKind,
    pub description: String,
    /// Base class names, in the order the FGD lists them.
    pub bases: Vec<String>,
    pub properties: Vec<Property>,
    pub inputs: Vec<Port>,
    pub outputs: Vec<Port>,
    /// The box `size()` gives, relative to the origin.
    pub size: Option<([f64; 3], [f64; 3])>,
    /// The editor colour `color()` gives.
    pub color: Option<[u8; 3]>,
    /// The model `studio("...")` shows for the entity, when it names one.
    pub studio: Option<String>,
    /// Whether `studio()` without a path shows the entity's own `model` key.
    pub studio_from_model: bool,
    /// The sprite `iconsprite("...")` shows, as a material path.
    pub icon: Option<String>,
    /// Whether `sprite()` without a path shows the entity's own `model` key as the game
    /// draws it, as for env_sprite.
    pub sprite_from_model: bool,
    /// Whether the model comes from `lightprop()`, which Hammer turns to point where the
    /// light shines, taking the pitch from the `pitch` key as the compiler does.
    pub light_prop: bool,
}

impl EntityClass {
    /// The keyvalue with this name, ignoring case.
    pub fn property(&self, name: &str) -> Option<&Property> {
        self.properties
            .iter()
            .find(|property| property.name.eq_ignore_ascii_case(name))
    }
}

/// Every entity class from a set of FGD files.
#[derive(Debug, Clone, Default)]
pub struct GameData {
    /// Classes by lower-case name, with base classes merged in.
    classes: HashMap<String, EntityClass>,
    /// The FGD files read, including `@include`d ones.
    pub files: Vec<PathBuf>,
}

impl GameData {
    /// Reads FGD files in order, following `@include`. Later files override classes of
    /// the same name, as in Hammer.
    pub fn load(paths: &[PathBuf]) -> Result<Self, String> {
        let mut raw = RawData::default();
        for path in paths {
            raw.read_file(path)?;
        }
        Ok(raw.resolve())
    }

    /// Parses FGD text that has no `@include`s. Used by tests and tools.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut raw = RawData::default();
        raw.parse(text, None)?;
        Ok(raw.resolve())
    }

    /// The class with this name, ignoring case.
    pub fn class(&self, name: &str) -> Option<&EntityClass> {
        self.classes.get(&name.to_ascii_lowercase())
    }

    /// Names of every point and brush class, sorted.
    pub fn class_names(&self, kind: ClassKind) -> Vec<&str> {
        let mut names: Vec<&str> = self
            .classes
            .values()
            .filter(|class| class.kind == kind)
            .map(|class| class.name.as_str())
            .collect();
        names.sort_unstable();
        names
    }

    /// How many classes there are, base classes included.
    pub fn len(&self) -> usize {
        self.classes.len()
    }

    /// Whether no class was read.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }
}

/// Picks the FGD Hammer would use for a game folder such as `.../GarrysMod/garrysmod`:
/// a file in the game's `bin` folder named after the game folder, or a known fallback.
pub fn default_fgd(game_dir: &Path) -> Option<PathBuf> {
    let bin = game_dir.parent()?.join("bin");
    let game = game_dir.file_name()?.to_string_lossy().to_ascii_lowercase();
    let mut names = vec![format!("{game}.fgd")];
    names.extend(["halflife2.fgd", "hl2mp.fgd", "cstrike.fgd", "base.fgd"].map(str::to_owned));
    names.into_iter().map(|name| bin.join(name)).find(|path| path.is_file())
}

/// Classes as written, before base classes are merged in.
#[derive(Default)]
struct RawData {
    classes: Vec<EntityClass>,
    files: Vec<PathBuf>,
    /// Canonical paths of the files read, so each is read once.
    seen: Vec<PathBuf>,
}

impl RawData {
    fn read_file(&mut self, path: &Path) -> Result<(), String> {
        let canonical = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        if self.seen.contains(&canonical) {
            return Ok(());
        }
        self.seen.push(canonical);
        self.files.push(path.to_owned());
        let bytes = fs::read(path).map_err(|error| format!("{}: {error}", path.display()))?;
        let text = String::from_utf8_lossy(&bytes);
        self.parse(&text, path.parent())
            .map_err(|error| format!("{}: {error}", path.display()))
    }

    fn parse(&mut self, text: &str, folder: Option<&Path>) -> Result<(), String> {
        let tokens = tokenize(text)?;
        let mut parser = Parser { tokens, at: 0 };
        while let Some(token) = parser.next() {
            let Token::Word(word) = token else {
                return Err(format!(
                    "line {}: expected @ at the start of a declaration",
                    parser.line()
                ));
            };
            let Some(directive) = word.strip_prefix('@') else {
                return Err(format!("line {}: unexpected \"{word}\"", parser.line()));
            };
            let directive = directive.to_ascii_lowercase();
            let kind = match directive.as_str() {
                "include" => {
                    let file = parser.string()?;
                    if let Some(folder) = folder {
                        self.read_file(&folder.join(file))?;
                    }
                    continue;
                }
                "baseclass" => ClassKind::Base,
                "solidclass" => ClassKind::Solid,
                "pointclass" | "npcclass" | "keyframeclass" | "moveclass" | "filterclass" => ClassKind::Point,
                // @mapsize(...), @MaterialExclusion [...], @AutoVisGroup = "..." [...] and
                // anything newer: skip to the end of the declaration.
                _ => {
                    parser.skip_declaration();
                    continue;
                }
            };
            self.classes.push(parser.class(kind)?);
        }
        Ok(())
    }

    fn resolve(self) -> GameData {
        // Base classes get their own namespace: base.fgd has both a `Light` base class
        // and a `light` point class, and names ignore case.
        let mut written = Written::default();
        for class in self.classes {
            let name = class.name.to_ascii_lowercase();
            match class.kind {
                ClassKind::Base => written.bases.insert(name, class),
                _ => written.concrete.insert(name, class),
            };
        }
        let mut classes = HashMap::new();
        for (name, class) in written.concrete.iter().chain(&written.bases) {
            if classes.contains_key(name) {
                continue;
            }
            let mut resolved = class.clone();
            let mut visiting = Vec::new();
            merge_bases(&written, &mut resolved, &mut visiting);
            classes.insert(name.clone(), resolved);
        }
        GameData {
            classes,
            files: self.files,
        }
    }
}

/// Classes as written, by lower-case name.
#[derive(Default)]
struct Written {
    bases: HashMap<String, EntityClass>,
    concrete: HashMap<String, EntityClass>,
}

impl Written {
    /// The class a `base(...)` name refers to: a base class, or else any class.
    fn base(&self, name: &str) -> Option<&EntityClass> {
        self.bases.get(name).or_else(|| self.concrete.get(name))
    }
}

/// Puts every base class's keyvalues, inputs and outputs before the class's own, keeping
/// the class's own version of anything it redefines, and inherits helpers it lacks.
fn merge_bases(written: &Written, class: &mut EntityClass, visiting: &mut Vec<String>) {
    let mut properties = Vec::new();
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    for base_name in class.bases.clone() {
        let key = base_name.to_ascii_lowercase();
        if visiting.contains(&key) {
            continue;
        }
        let Some(base) = written.base(&key) else { continue };
        let mut base = base.clone();
        visiting.push(key);
        merge_bases(written, &mut base, visiting);
        visiting.pop();
        properties.extend(base.properties);
        inputs.extend(base.inputs);
        outputs.extend(base.outputs);
        class.size = class.size.or(base.size);
        class.color = class.color.or(base.color);
        if class.studio.is_none() && !class.studio_from_model {
            class.studio = base.studio;
            class.studio_from_model = base.studio_from_model;
            class.light_prop = base.light_prop;
        }
        if class.icon.is_none() {
            class.icon = base.icon;
        }
        class.sprite_from_model |= base.sprite_from_model;
    }
    properties.append(&mut class.properties);
    inputs.append(&mut class.inputs);
    outputs.append(&mut class.outputs);
    class.properties = last_of_each(properties, |property| property.name.to_ascii_lowercase());
    class.inputs = last_of_each(inputs, |port| port.name.to_ascii_lowercase());
    class.outputs = last_of_each(outputs, |port| port.name.to_ascii_lowercase());
}

/// Keeps one item per key: the last one, at the position of the first.
fn last_of_each<T>(items: Vec<T>, key: impl Fn(&T) -> String) -> Vec<T> {
    let mut order: Vec<String> = Vec::new();
    let mut latest: HashMap<String, T> = HashMap::new();
    for item in items {
        let name = key(&item);
        if !latest.contains_key(&name) {
            order.push(name.clone());
        }
        latest.insert(name, item);
    }
    order.into_iter().filter_map(|name| latest.remove(&name)).collect()
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// A quoted string, with `"a" + "b"` already joined.
    Text(String),
    /// Anything else that is not punctuation: names, numbers, `@PointClass`.
    Word(String),
    /// One of `= : [ ] ( ) ,`.
    Punct(char),
}

fn tokenize(text: &str) -> Result<Vec<(Token, usize)>, String> {
    let mut tokens: Vec<(Token, usize)> = Vec::new();
    let mut chars = text.chars().peekable();
    let mut line = 1;
    // Set after a `+` that follows a string, so the next string is appended to it.
    let mut joining = false;
    while let Some(&c) = chars.peek() {
        match c {
            '\n' => {
                line += 1;
                chars.next();
            }
            c if c.is_whitespace() => {
                chars.next();
            }
            '/' => {
                chars.next();
                if chars.peek() == Some(&'/') {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            line += 1;
                            break;
                        }
                    }
                } else {
                    tokens.push((Token::Word("/".to_owned()), line));
                }
            }
            '"' => {
                chars.next();
                let start = line;
                let mut value = String::new();
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\n') => {
                            line += 1;
                            value.push('\n');
                        }
                        Some(c) => value.push(c),
                        None => return Err(format!("line {start}: string is not closed")),
                    }
                }
                // Descriptions write line breaks as `\n`.
                let value = value.replace("\\n", "\n");
                match tokens.last_mut() {
                    Some((Token::Text(previous), _)) if joining => previous.push_str(&value),
                    _ => tokens.push((Token::Text(value), start)),
                }
                joining = false;
            }
            '+' if matches!(tokens.last(), Some((Token::Text(_), _))) => {
                chars.next();
                joining = true;
            }
            '=' | ':' | '[' | ']' | '(' | ')' | ',' => {
                chars.next();
                tokens.push((Token::Punct(c), line));
            }
            _ => {
                let mut word = String::new();
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() || "=:[](),\"".contains(c) || (c == '/' && word.ends_with('/')) {
                        break;
                    }
                    word.push(c);
                    chars.next();
                }
                tokens.push((Token::Word(word), line));
            }
        }
    }
    Ok(tokens)
}

struct Parser {
    tokens: Vec<(Token, usize)>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.at).map(|(token, _)| token)
    }

    fn next(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.at).map(|(token, _)| token.clone());
        self.at += 1;
        token
    }

    fn line(&self) -> usize {
        self.tokens
            .get(self.at.saturating_sub(1))
            .or(self.tokens.last())
            .map_or(0, |(_, line)| *line)
    }

    fn is_punct(&self, c: char) -> bool {
        self.peek() == Some(&Token::Punct(c))
    }

    fn expect(&mut self, c: char) -> Result<(), String> {
        if self.is_punct(c) {
            self.at += 1;
            Ok(())
        } else {
            Err(format!(
                "line {}: expected '{c}', found {:?}",
                self.line() + 1,
                self.peek()
            ))
        }
    }

    fn string(&mut self) -> Result<String, String> {
        match self.next() {
            Some(Token::Text(text) | Token::Word(text)) => Ok(text),
            other => Err(format!("line {}: expected a string, found {other:?}", self.line())),
        }
    }

    /// A string or word if one comes next.
    fn value(&mut self) -> Option<String> {
        match self.peek() {
            Some(Token::Text(text) | Token::Word(text)) => {
                let text = text.clone();
                self.at += 1;
                Some(text)
            }
            _ => None,
        }
    }

    /// Skips a declaration this parser does not use, up to and including its bracketed
    /// body if it has one.
    fn skip_declaration(&mut self) {
        loop {
            match self.peek() {
                None => return,
                Some(Token::Word(word)) if word.starts_with('@') => return,
                Some(Token::Punct('(')) => self.skip_group('(', ')'),
                Some(Token::Punct('[')) => {
                    self.skip_group('[', ']');
                    return;
                }
                _ => self.at += 1,
            }
        }
    }

    fn skip_group(&mut self, open: char, close: char) {
        let mut depth = 0;
        while let Some(token) = self.next() {
            match token {
                Token::Punct(c) if c == open => depth += 1,
                Token::Punct(c) if c == close => {
                    depth -= 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => {}
            }
        }
    }

    /// The words inside a helper's parentheses, split at commas.
    fn helper_arguments(&mut self) -> Result<Vec<Vec<String>>, String> {
        self.expect('(')?;
        let mut groups = vec![Vec::new()];
        loop {
            match self.next() {
                Some(Token::Punct(')')) => break,
                Some(Token::Punct(',')) => groups.push(Vec::new()),
                Some(Token::Text(text) | Token::Word(text)) => groups.last_mut().expect("never empty").push(text),
                Some(Token::Punct(_)) => {}
                None => return Err("helper arguments are not closed".to_owned()),
            }
        }
        groups.retain(|group| !group.is_empty());
        Ok(groups)
    }

    fn class(&mut self, kind: ClassKind) -> Result<EntityClass, String> {
        let mut class = EntityClass {
            name: String::new(),
            kind,
            description: String::new(),
            bases: Vec::new(),
            properties: Vec::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            size: None,
            color: None,
            studio: None,
            studio_from_model: false,
            icon: None,
            sprite_from_model: false,
            light_prop: false,
        };
        // Helpers up to '='.
        while !self.is_punct('=') {
            let helper = match self.next() {
                Some(Token::Word(word)) => word.to_ascii_lowercase(),
                other => {
                    return Err(format!(
                        "line {}: expected a helper or '=', found {other:?}",
                        self.line()
                    ))
                }
            };
            let arguments = if self.is_punct('(') {
                self.helper_arguments()?
            } else {
                Vec::new()
            };
            let numbers = |group: &Vec<String>| -> Vec<f64> { group.iter().filter_map(|n| n.parse().ok()).collect() };
            match helper.as_str() {
                "base" => class.bases = arguments.into_iter().flatten().collect(),
                "size" => {
                    let vectors: Vec<Vec<f64>> = arguments.iter().map(numbers).collect();
                    class.size = match vectors.as_slice() {
                        [a, b] if a.len() == 3 && b.len() == 3 => Some(([a[0], a[1], a[2]], [b[0], b[1], b[2]])),
                        [a] if a.len() == 3 => Some((
                            [-a[0] / 2.0, -a[1] / 2.0, -a[2] / 2.0],
                            [a[0] / 2.0, a[1] / 2.0, a[2] / 2.0],
                        )),
                        _ => None,
                    };
                }
                "color" => {
                    let values = arguments.first().map(numbers).unwrap_or_default();
                    if values.len() == 3 {
                        class.color = Some([values[0] as u8, values[1] as u8, values[2] as u8]);
                    }
                }
                "studio" | "studioprop" | "lightprop" => {
                    class.light_prop = helper == "lightprop";
                    match arguments.first().and_then(|group| group.first()) {
                        Some(path) => class.studio = Some(path.clone()),
                        None => class.studio_from_model = true,
                    }
                }
                "iconsprite" => class.icon = arguments.first().and_then(|group| group.first()).cloned(),
                "sprite" => match arguments.first().and_then(|group| group.first()) {
                    Some(path) => class.icon = Some(path.clone()),
                    None => class.sprite_from_model = true,
                },
                _ => {}
            }
        }
        self.expect('=')?;
        class.name = self.string()?;
        if self.is_punct(':') {
            self.at += 1;
            class.description = self.value().unwrap_or_default();
        }
        if self.is_punct('[') {
            self.at += 1;
            self.class_body(&mut class)?;
        }
        Ok(class)
    }

    fn class_body(&mut self, class: &mut EntityClass) -> Result<(), String> {
        loop {
            let name = match self.next() {
                Some(Token::Punct(']')) => return Ok(()),
                Some(Token::Word(word)) => word,
                Some(Token::Text(text)) => text,
                Some(other) => return Err(format!("line {}: unexpected {other:?} in {}", self.line(), class.name)),
                None => return Err(format!("class {} is not closed", class.name)),
            };
            let lower = name.to_ascii_lowercase();
            if (lower == "input" || lower == "output") && matches!(self.peek(), Some(Token::Word(_))) {
                let port_name = self.string()?;
                let kind = self.parenthesised_type()?;
                let description = if self.is_punct(':') {
                    self.at += 1;
                    self.value().unwrap_or_default()
                } else {
                    String::new()
                };
                let port = Port {
                    name: port_name,
                    kind,
                    description,
                };
                if lower == "input" {
                    class.inputs.push(port);
                } else {
                    class.outputs.push(port);
                }
                continue;
            }
            let kind = self.parenthesised_type()?;
            let mut read_only = false;
            while let Some(Token::Word(word)) = self.peek() {
                match word.to_ascii_lowercase().as_str() {
                    "readonly" => read_only = true,
                    "report" => {}
                    _ => break,
                }
                self.at += 1;
            }
            // Up to three ':'-separated fields: display name, default and description.
            let mut fields: Vec<String> = Vec::new();
            while fields.len() < 3 && self.is_punct(':') {
                self.at += 1;
                fields.push(self.value().unwrap_or_default());
            }
            let mut choices = Vec::new();
            if self.is_punct('=') {
                self.at += 1;
                self.expect('[')?;
                while !self.is_punct(']') {
                    let value = self.string()?;
                    self.expect(':')?;
                    let display = self.value().unwrap_or_default();
                    let mut default_on = false;
                    if self.is_punct(':') {
                        self.at += 1;
                        default_on = self.value().is_some_and(|value| value.trim() == "1");
                    }
                    choices.push(Choice {
                        value,
                        name: display,
                        default_on,
                    });
                }
                self.at += 1;
            }
            let mut fields = fields.into_iter();
            let display = fields
                .next()
                .filter(|display| !display.is_empty())
                .unwrap_or_else(|| name.clone());
            class.properties.push(Property {
                name,
                kind,
                display,
                default: fields.next().unwrap_or_default(),
                description: fields.next().unwrap_or_default(),
                choices,
                read_only,
            });
        }
    }

    fn parenthesised_type(&mut self) -> Result<String, String> {
        self.expect('(')?;
        // A leading `*` marks a keyvalue Hammer treats specially; the type is the same.
        let kind = self.string()?.trim_start_matches('*').to_ascii_lowercase();
        self.expect(')')?;
        Ok(kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
@mapsize(-16384, 16384)
// A comment.
@BaseClass = Targetname
[
	targetname(target_source) : "Name" : : "The name that other entities refer to this entity by."
	input Kill(void) : "Removes this entity from the world."
	output OnUser1(void) : "Fired in response to FireUser1 input."
]

@BaseClass = Angles [ angles(angle) : "Pitch Yaw Roll (Y Z X)" : "0 0 0" : "Orientation." ]

@PointClass base(Targetname, Angles) size(-16 -16 0, 16 16 72) color(0 255 0)
	studio("models/editor/playerstart.mdl") = info_player_start :
	"Where the player " + "spawns."
[
	spawnflags(flags) =
	[
		1: "Master" : 0
		2: "Other" : 1
	]
	rendermode(choices) : "Render Mode" : 0 : "Used to set a non-standard rendering mode." =
	[
		0: "Normal"
		1: "Color"
	]
	targetname(target_source) readonly : "Spawn name"
]

@SolidClass base(Targetname) = func_detail_blocker : "Blocks detail sprites." []
@AutoVisGroup = "Brushes" [ "Triggers" [ "trigger_once" ] ]
"#;

    #[test]
    fn parses_classes_with_bases_flags_and_choices() {
        let data = GameData::parse(SAMPLE).unwrap();
        let start = data.class("INFO_PLAYER_START").unwrap();
        assert_eq!(start.kind, ClassKind::Point);
        assert_eq!(start.description, "Where the player spawns.");
        assert_eq!(start.size, Some(([-16.0, -16.0, 0.0], [16.0, 16.0, 72.0])));
        assert_eq!(start.color, Some([0, 255, 0]));
        assert_eq!(start.studio.as_deref(), Some("models/editor/playerstart.mdl"));
        let names: Vec<&str> = start.properties.iter().map(|property| property.name.as_str()).collect();
        assert_eq!(names, ["targetname", "angles", "spawnflags", "rendermode"]);
        let targetname = start.property("targetname").unwrap();
        assert_eq!(targetname.display, "Spawn name");
        assert!(targetname.read_only);
        let angles = start.property("angles").unwrap();
        assert_eq!((angles.kind.as_str(), angles.default.as_str()), ("angle", "0 0 0"));
        let flags = &start.property("spawnflags").unwrap().choices;
        assert_eq!(flags.len(), 2);
        assert!(!flags[0].default_on && flags[1].default_on);
        let render = start.property("rendermode").unwrap();
        assert_eq!(render.default, "0");
        assert_eq!(render.choices[1].name, "Color");
        assert_eq!(start.inputs[0].name, "Kill");
        assert_eq!(start.outputs[0].name, "OnUser1");
        assert_eq!(data.class_names(ClassKind::Solid), ["func_detail_blocker"]);
    }

    #[test]
    fn reads_installed_game_fgds() {
        let Some(dir) = std::env::var_os("MALLET_GAME_DIR") else {
            return;
        };
        let Some(fgd) = default_fgd(Path::new(&dir)) else {
            return;
        };
        let started = std::time::Instant::now();
        let data = GameData::load(&[fgd]).unwrap();
        eprintln!(
            "{} classes from {:?} in {:?}",
            data.len(),
            data.files,
            started.elapsed()
        );
        let prop = data.class("prop_static").unwrap();
        assert!(prop.studio_from_model);
        assert!(prop.property("model").is_some());
        let light = data.class("light").unwrap();
        assert!(light
            .property("_light")
            .is_some_and(|property| property.kind == "color255"));
        assert!(data.class("info_player_start").unwrap().studio.is_some());
        assert!(data
            .class("trigger_multiple")
            .unwrap()
            .outputs
            .iter()
            .any(|port| port.name == "OnTrigger"));
    }
}
