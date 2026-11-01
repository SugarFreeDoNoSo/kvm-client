"""Apply reviewed Android settings after Tauri generates its ignored native project."""
from pathlib import Path

root = Path(__file__).resolve().parent.parent
gradle = root / "src-tauri/gen/android/app/build.gradle.kts"
source = gradle.read_text()
# Pin the SDK to the version installed by CI, independently of CLI template updates.
import re
source, count = re.subn(r"compileSdk\s*=\s*\d+", "compileSdk = 36", source)
if count != 1:
    raise SystemExit("Unexpected Tauri Android compileSdk template")
source = re.sub(r"targetSdk\s*=\s*\d+", "targetSdk = 36", source)
gradle.write_text(source)

manifest = root / "src-tauri/gen/android/app/src/main/AndroidManifest.xml"
source = manifest.read_text()
if 'android:windowSoftInputMode=' not in source:
    source = source.replace("<activity", '<activity android:windowSoftInputMode="adjustResize"', 1)
manifest.write_text(source)
