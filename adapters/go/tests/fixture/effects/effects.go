// Package effects is the adapter layer: it is allowed to touch the filesystem
// and spawn subprocesses. The effect leaves live here, at the boundary — the
// functional core reaches them only through this package.
package effects

import (
	"os"
	"os/exec"
)

// ReadConfig performs a real filesystem effect — the leaf the analysis seeds as
// an fs root (os.ReadFile).
func ReadConfig(pathToConfig string) string {
	data, err := os.ReadFile(pathToConfig)
	if err != nil {
		return ""
	}
	return string(data)
}

// RunTool performs a real subprocess effect — seeded as a process root
// (os/exec.Command).
func RunTool(name string) int {
	out, err := exec.Command(name, "--version").Output()
	_ = out
	if err != nil {
		return 1
	}
	return 0
}
