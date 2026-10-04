"""Small text helpers shared by resolution, acquisition and grounding."""

from __future__ import annotations

import re
import unicodedata

STOPWORDS = frozenset({"a", "an", "and", "in", "of", "the"})


def fold(text: str) -> str:
    """Lowercase, strip accents, and turn punctuation runs into single spaces."""
    decomposed = unicodedata.normalize("NFKD", text)
    plain = "".join(ch for ch in decomposed if not unicodedata.combining(ch))
    return re.sub(r"[\W_]+", " ", plain.casefold()).strip()


def tokens(text: str) -> set[str]:
    """Folded words of ``text`` without stopwords."""
    return {word for word in fold(text).split() if word not in STOPWORDS}


def slugify(text: str, max_length: int = 48) -> str:
    """ASCII, filesystem-safe ``[a-z0-9_]`` slug. May be empty for non-Latin text."""
    ascii_text = unicodedata.normalize("NFKD", text).encode("ascii", "ignore").decode("ascii")
    slug = re.sub(r"[^a-z0-9]+", "_", ascii_text.lower()).strip("_")
    return slug[:max_length].strip("_")
