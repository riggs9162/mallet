//! Hammer's Run Map dialog in its normal mode: whether and how VBSP, VVIS and VRAD run,
//! HDR lighting, starting the game and the game's parameters. Its choices become the
//! "Run Map" compile preset, which the compile window shows and Shift+F9 runs again.

use serde::{Deserialize, Serialize};

use super::{save_presets, Preset, Step, StepKind};
use crate::editor::Editor;

/// The preset the Run Map dialog compiles with.
pub const RUN_MAP_PRESET: &str = "Run Map";

/// Run BSP's choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunBsp {
    No,
    #[default]
    Normal,
    /// VBSP's `-onlyents`: only the entities of the last compile change.
    OnlyEntities,
}

/// Run VIS's and Run RAD's choices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPass {
    No,
    #[default]
    Normal,
    /// The tool's `-fast`: rough results for testing.
    Fast,
}

/// What the Run Map dialog remembers between compiles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunMapOptions {
    pub bsp: RunBsp,
    pub vis: RunPass,
    pub rad: RunPass,
    /// Light the map for HDR as well as LDR, which takes about twice as long.
    pub hdr: bool,
    /// Leave the game alone after compiling.
    pub dont_run_game: bool,
    /// More arguments for the game, such as `-toconsole -dev -console +sv_lan 1`.
    pub game_parameters: String,
}

impl Default for RunMapOptions {
    fn default() -> Self {
        Self {
            bsp: RunBsp::Normal,
            vis: RunPass::Normal,
            rad: RunPass::Normal,
            hdr: false,
            dont_run_game: false,
            game_parameters: String::new(),
        }
    }
}

impl RunMapOptions {
    /// The compile these choices ask for: the chosen tools, then copying the BSP into the
    /// game and, unless told not to, starting the game on it.
    pub fn preset(&self) -> Preset {
        let mut steps = Vec::new();
        match self.bsp {
            RunBsp::No => {}
            RunBsp::Normal => steps.push(Step::new(StepKind::Vbsp)),
            RunBsp::OnlyEntities => steps.push(Step::new(StepKind::Vbsp).with(&["-onlyents"])),
        }
        match self.vis {
            RunPass::No => {}
            RunPass::Normal => steps.push(Step::new(StepKind::Vvis)),
            RunPass::Fast => steps.push(Step::new(StepKind::Vvis).with(&["-fast"])),
        }
        let range = if self.hdr { "-both" } else { "-ldr" };
        match self.rad {
            RunPass::No => {}
            RunPass::Normal => steps.push(Step::new(StepKind::Vrad).with(&[range])),
            RunPass::Fast => steps.push(Step::new(StepKind::Vrad).with(&["-fast", range])),
        }
        steps.push(Step::new(StepKind::CopyToGame));
        let mut game = Step::new(StepKind::RunGame);
        game.extra_args = self.game_parameters.trim().to_owned();
        game.enabled = !self.dont_run_game;
        steps.push(game);
        Preset {
            name: RUN_MAP_PRESET.to_owned(),
            description: "Made by the Run Map dialog (F9), as Hammer's normal Run Map compiles.".to_owned(),
            steps,
        }
    }
}

impl Editor {
    /// Compiles with the Run Map dialog's choices: they become the "Run Map" preset,
    /// saved with the others, and the compile starts.
    pub fn run_map(&mut self) -> Result<(), String> {
        let preset = self.config.compile.run_map.preset();
        match self.compile_presets.iter_mut().find(|existing| existing.name == RUN_MAP_PRESET) {
            Some(existing) => *existing = preset,
            None => self.compile_presets.push(preset),
        }
        if let Err(error) = save_presets(&self.compile_presets) {
            self.message = format!("Could not save the compile presets: {error}");
        }
        self.start_compile(RUN_MAP_PRESET)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tutorial_settings_compile_everything_and_start_the_game() {
        let options = RunMapOptions {
            hdr: true,
            game_parameters: "-toconsole -dev -console +sv_lan 1".to_owned(),
            ..RunMapOptions::default()
        };
        let preset = options.preset();
        let kinds: Vec<StepKind> = preset.steps.iter().map(|step| step.kind).collect();
        assert_eq!(
            kinds,
            [
                StepKind::Vbsp,
                StepKind::Vvis,
                StepKind::Vrad,
                StepKind::CopyToGame,
                StepKind::RunGame
            ]
        );
        assert!(preset.steps[2].option("-both").is_some());
        assert!(preset.steps[4].enabled);
        assert_eq!(preset.steps[4].extra_args, "-toconsole -dev -console +sv_lan 1");
        let entities = RunMapOptions {
            bsp: RunBsp::OnlyEntities,
            vis: RunPass::No,
            rad: RunPass::No,
            dont_run_game: true,
            ..RunMapOptions::default()
        }
        .preset();
        assert!(entities.steps[0].option("-onlyents").is_some());
        assert!(!entities.steps.last().unwrap().enabled);
    }
}
