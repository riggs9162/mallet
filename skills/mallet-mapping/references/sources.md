# Sources and further reading

This skill condenses community and developer practice. Where sources disagreed, or a
figure varies by game, the text says to test in game. Engine limits and jump heights in
particular change between games, gamemodes and compiler versions: the compile log and
the game are the authority, not this file.

## Valve Developer Community

The reference for entities, tools and limits. `developer.valvesoftware.com/wiki/` then:

- `Dimensions_(Half-Life_2_and_Counter-Strike:_Source)`, `Player_Hull`, `Unit_scales`
- `Tool_textures_(Source)`, `Toolsnodraw`, `Brush`
- `Visibility_optimization`, `Func_detail`, `Hint_brush`, `Areaportal_tutorial`, `Leak`
- `Prop_Types_Overview`, `Prop_static`, `Displacement`
- `Light_environment`, `Light`, `Light_spot`, `Falloff`, `Lightmap`, `VRAD`
- `Intermediate_Lighting`, `Advanced_HDR`, `Env_tonemap_controller`
- `Env_fog_controller`, `Fog_tutorial`, `Color_correction_(entity)`
- `Cubemaps`, `Soundscape`, `Env_soundscape`, `3D_Skybox`, `List_of_Skyboxes`
- `Inputs_and_Outputs`, `Logic_relay`, `Ent_fire`, `Ent_messages_draw`
- `Prop_door_rotating`, `Func_tracktrain`, `WiseElevators`
- `Compile_Errors`, `Entity_limit`, `BSPZIP`, `Pre-publication_evaluation`
- `Half-Life_2_level_design`, `Multi-Player_Mapping_Tips`, `Adding_More_Detail_to_a_map`
- `Trouble_in_Terrorist_Town_Mapping`, `Map_Prefixes`

## Garry's Mod

- Facepunch wiki: `wiki.facepunch.com/gmod/` then `Map_Testing`, `navmesh`,
  `Workshop_Addon_Creation`, `Steam_Workshop_Rules`, `Player:GetHull`
- Update notes that raised map limits: `gmod.facepunch.com/changelist/2798` (2019),
  `gmod.facepunch.com/news/march-2024-update`, `gmod.facepunch.com/changelist/4006` (2025)
- Trouble in Terrorist Town re-arm and map entities: `troubleinterroristtown.com`

## Craft and theory

- World of Level Design (Alex Galuzin), `worldofleveldesign.com`: Source player scale and
  dimensions, the blockout process, collecting reference, silhouette, and the Counter-
  Strike "abandoned house" workflow from blockout through detailing and lighting.
- The Level Design Book, `book.leveldesignbook.com`: blockout, massing, lighting, metrics.
- Game Developer: "Composition in Level Design"; "Analyzing level layouts to improve
  level design in competitive FPS"; "Practical guide on first-person level design";
  "Level design: do your research, where's the toilet"; "Dust II to Dust II: how Valve
  rebuilt a classic".
- 80 Level: Steve Lee's eight common problems with level layouts; "Attack the block";
  environment design on modularity, colour and composition.
- Valve: "Valve's Design Process for Creating Half-Life 2" (GDC 2006), and the developer
  commentary tracks in Half-Life 2: Episode One and Two, Portal and Team Fortress 2.
- Communities with tutorial archives and feedback forums: TF2Maps (`tf2maps.net`),
  Mapcore (`mapcore.org`), TWHL (`twhl.info`), Interlopers, r/hammer, r/SourceEngine.
- Video: TopHATTwaffle's Hammer tutorial series and 3kliksphilip's mapping series cover
  optimisation, lighting and detailing step by step.

## Tools named in this skill

- Compile Pal (`github.com/ruarai/CompilePal`): compile front end with automatic packing.
- BSPSource (`github.com/ata4/bspsrc`): decompiler, for studying how released maps are
  built. Do not republish other people's work.
- VIDE: content scanning and packing. Pakrat is outdated and can corrupt repacked maps.
- Community compilers (the `++` tool builds, Slammin' tools) add flags such as ambient
  occlusion and prop bounce; a flag is only valid if the compile log accepts it.
