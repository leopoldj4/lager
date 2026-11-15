// Removes the containers and the data of the tests: `bun run down`.

import { rmSync } from "node:fs"
import { down, root } from "./services"

await down()
rmSync(root, { recursive: true, force: true })
