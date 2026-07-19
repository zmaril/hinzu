"""Module-level SQLAlchemy — the flagship of the reference rung.

The engine and session factory are built at IMPORT time, at module scope, so a
call-only walk (which only descends function bodies) never saw them: this file
emitted no edges at all. The reference rung attributes module-scope use to this
file's synthetic `<module>` node, and `create_engine` / `sessionmaker` surface
`db` — exactly the import-time database effect the SQLAlchemy annotation pack was
authored for but could not fire under call-only."""

from __future__ import annotations

from sqlalchemy import create_engine
from sqlalchemy.orm import sessionmaker

ENGINE = create_engine("sqlite:///demo.db")
SessionLocal = sessionmaker(bind=ENGINE)
