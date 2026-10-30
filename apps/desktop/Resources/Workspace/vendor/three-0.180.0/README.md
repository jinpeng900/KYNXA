# Three.js 0.180.0 (r180)

Official npm release: https://registry.npmjs.org/three/-/three-0.180.0.tgz

Locked version: `0.180.0`. npm tarball integrity:
`sha512-o+qycAMZrh+TsE01GqWUxUIKR1AL0S8pq7zDkYOQw8GqfX8b8VoCKYUoHbhiX5j+7hr8XsuHDVU6+gkQJQKg9w==`

Included: official minified WebGL/core ESM builds, GLTF/OBJ/STL/MTL loaders,
OrbitControls, BufferGeometryUtils, RoomEnvironment, and the upstream MIT LICENSE.
Only modification to addons: bare `from 'three'` imports were changed to
`from '../../build/three.module.min.js'` so the application works offline without
an import map, CDN, package manager, or dynamic remote code.

Draco, KTX2 and Meshopt decoder binaries are not included. Assets requiring those
optional extensions receive an actionable error instead of downloading code.
