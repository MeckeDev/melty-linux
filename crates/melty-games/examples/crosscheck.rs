use melty_games::*;
fn main(){
assert_eq!(slug_for("Hades II"), "hades-ii", "slug 'Hades II'");
assert_eq!(slug_for("The Witcher® 3: Wild Hunt"), "the-witcher-3-wild-hunt", "slug 'The Witcher® 3: Wild Hunt'");
assert_eq!(slug_for("SkyrimSE.exe"), "skyrimse", "slug 'SkyrimSE.exe'");
assert_eq!(slug_for("  --Half-Life: Alyx--  "), "half-life-alyx", "slug '  --Half-Life: Alyx--  '");
assert_eq!(slug_for("ELDEN RING"), "elden-ring", "slug 'ELDEN RING'");
assert_eq!(slug_for("***"), "", "slug '***'");
assert_eq!(slug_for("İstanbul ÄÖÜ ß Café"), "i-stanbul-caf", "slug 'İstanbul ÄÖÜ ß Café'");
assert_eq!(slug_for("Cyberpunk 2077"), "cyberpunk-2077", "slug 'Cyberpunk 2077'");
assert_eq!(slug_for("DARK SOULS™ III"), "dark-souls-iii", "slug 'DARK SOULS™ III'");
assert_eq!(slug_for("Pokémon_Café.EXE"), "pok-mon-caf", "slug 'Pokémon_Café.EXE'");
assert_eq!(slug_for("a  b\tc"), "a-b-c", "slug 'a  b\tc'");
assert_eq!(slug_for("日本語 Game 2"), "game-2", "slug '日本語 Game 2'");
assert_eq!(installation_id("portal-2","Steam","/Home/U/.local/share/Steam/steamapps/common/Portal 2","620"), "fc0276de5cf0adb2578a95a3");
assert_eq!(installation_id("grand-theft-auto-v","Steam","/mnt/Games/SteamLibrary/steamapps/common/Grand Theft Auto V","271590"), "9feaae8045d2ffc74c8bc781");
assert_eq!(installation_id("pokémon","Steam","/home/ü/Spiele/Pokémon","1"), "87284ae6051009e331ad9494");
println!("Kreuzprüfung gegen JS-Original: OK");}