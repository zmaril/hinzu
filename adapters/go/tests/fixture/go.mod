// A minimal module marker so `hinzu check` routes this fixture to the Go
// adapter (a `go.mod` selects Go) and so gopls has a module to typecheck. The
// fixture uses only the standard library, so no dependencies are fetched.
module hinzu.example/gofixture

go 1.23
