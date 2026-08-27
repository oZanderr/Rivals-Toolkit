# Bundled bypass binaries

## dsound.dll

Oxide ASI Loader (`oxiloader`) by oZanderr, redistributed unmodified.
Source: https://github.com/oZanderr/oxiloader
Build: v0.3.0 release asset `dsound.dll`.
SHA-512: ca72085ed583f86b096c10e691781e08d46273b7e55fae9531a9d321ce406a86d16f61a21163474b041fd63c1c1c86975e42c2844ca3d71fbd24d06d22713f42

v0.2.1 crashed the game and is treated as superseded by `mods::bypass`, which replaces it on
Install. Its SHA-256 is listed there; add this build's hash to that list when it is next replaced.

Licensed under the MIT License, Copyright (c) 2026 oZanderr. The full license
text is in `oxiloader-LICENSE.txt` alongside this file.

Installed next to the game executable as `dsound.dll`; it loads `.asi`
plugins from the `plugins` subfolder. The same release builds the loader under
other proxy names as well, including the `version.dll` that earlier toolkit
versions installed.

## MarvelRivalsUTOCSignatureBypass.asi

The community pak/utoc signature bypass payload, redistributed unmodified from
its original Nexus Mods release. No license text accompanies the binary.
SHA-512: 73218a0f4762781830c6e289fdcc0a64d6b59bc2381b2ac29bcdbae124149dd4206bb10c2bfa8f209282fd1e7d3808e2eb0e0f0295bf95050deff67ab7e5abcc
Size: 44,032 bytes.

Installed into the `plugins` subfolder next to the loader.

Ship this exact build. The toolkit previously installed its own payload
(oZanderr/rivals-sigbypass, `RivalsSigBypass.asi`) and the game began flagging
it, while this original binary keeps loading; a rebuild is not a substitute.
