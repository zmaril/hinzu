// Package core is the functional core: no filesystem, network, or subprocess
// effects are allowed here, however deep the call chain. Parse and CountKeys are
// genuinely pure; LoadAndSummarize and BuildAndReport are the leaks — they reach
// the filesystem and spawn a subprocess through the effects adapter, so the
// policy must flag them.
package core

import (
	"fmt"
	"strings"

	"hinzu.example/gofixture/effects"
)

// Parse turns a newline-separated "key=value" body into a map. Pure string
// algebra — nothing here touches the outside world.
func Parse(text string) map[string]string {
	out := map[string]string{}
	for _, line := range strings.Split(text, "\n") {
		key, value, ok := strings.Cut(line, "=")
		if ok {
			out[strings.TrimSpace(key)] = strings.TrimSpace(value)
		}
	}
	return out
}

// CountKeys counts the entries in a parsed config. Pure.
func CountKeys(config map[string]string) int {
	return len(config)
}

// LoadAndSummarize looks like plain core logic, but transitively performs
// filesystem I/O through the adapter — the leak the policy must flag.
func LoadAndSummarize(pathToConfig string) int {
	config := Parse(effects.ReadConfig(pathToConfig))
	return CountKeys(config)
}

// BuildAndReport transitively spawns a subprocess through the adapter — the
// second leak the policy must flag.
func BuildAndReport(tool string) string {
	code := effects.RunTool(tool)
	return fmt.Sprintf("%s exited %d", tool, code)
}
