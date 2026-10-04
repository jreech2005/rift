import httpx
import pytest

from rift_canon.acquire import (
    MAX_WIKI_REQUESTS,
    acquire_canon,
    billed_cast,
    normalize_wiki_text,
)
from rift_canon.canon_packet import CanonPacket, Classification, MediaType, SourceType
from rift_canon.errors import CanonAcquisitionError, ProviderError
from rift_canon.providers.tmdb_catalog import TMDBCatalog
from rift_canon.providers.wikipedia import USER_AGENT, WikipediaProvider
from rift_canon.resolve import resolve_title
from tests.conftest import SECRET_VALUES
from tests.support import (
    MAIN_EXTRACT,
    NOW,
    WIKIDATA_HOST,
    WIKIPEDIA_HOST,
    FakeWeb,
    fake_settings,
    fixture,
    run,
    saved_packet,
    wiki_page,
)


def _acquire(title: str = "Breaking Bad", web: FakeWeb | None = None) -> CanonPacket:
    web = web or FakeWeb()
    settings = fake_settings()

    async def go() -> CanonPacket:
        async with web.client() as client:
            universe = await resolve_title(client, TMDBCatalog(settings), title)
            wiki = WikipediaProvider(settings, delay=0, retry_delay=0)
            return await acquire_canon(client, TMDBCatalog(settings), wiki, universe, now=NOW)

    return run(go())


def test_normalization_keeps_story_sections_only() -> None:
    text, stats = normalize_wiki_text(MAIN_EXTRACT, max_chars=10_000)
    assert "## Premise" in text
    assert "## Cast and characters" in text
    assert "Walter White teaches chemistry" in text
    # Real-world sections go, together with their subsections.
    for gone in ("Production", "Casting", "filmed in Albuquerque", "Reception", "References"):
        assert gone not in text
    assert stats["sections_dropped"] == ["Production", "Reception", "References"]
    # Whitespace is tidied: no runs of spaces, no triple newlines.
    assert "Jesse Pinkman, a former student" in text
    assert "  " not in text
    assert "\n\n\n" not in text
    assert stats["truncated"] is False


def test_normalization_truncates_on_a_paragraph_boundary() -> None:
    extract = "\n\n".join(f"Paragraph {n} " + "word " * 40 for n in range(20))
    text, stats = normalize_wiki_text(extract, max_chars=1_000)
    assert len(text) <= 1_000
    assert stats["truncated"] is True
    assert stats["original_chars"] > 1_000
    assert text.endswith("word")
    assert text.count("Paragraph") == len(text.split("\n\n"))


def test_tv_cast_is_ordered_by_episodes_then_billing() -> None:
    cast = billed_cast(fixture("tmdb_tv_1396.json"), MediaType.TV)
    assert [member.character for member in cast] == [
        "Walter White",
        "Jesse Pinkman",
        "Skyler White",
        "Walter White Jr.",
        "Hank Schrader",
        "Saul Goodman",
        "Gustavo Fring",
    ]
    assert cast[0].actor == "Bryan Cranston"
    assert cast[5].episodes == 43


def test_movie_cast_names_are_cleaned() -> None:
    cast = billed_cast(fixture("tmdb_movie_603.json"), MediaType.MOVIE)
    # "Thomas A. Anderson / Neo", "Agent Smith (voice)" and "Self".
    assert [member.character for member in cast] == [
        "Thomas A. Anderson",
        "Morpheus",
        "Trinity",
        "Agent Smith",
    ]
    assert all(member.episodes is None for member in cast)


def test_cast_tolerates_missing_or_odd_data() -> None:
    assert billed_cast({}, MediaType.TV) == []
    assert billed_cast({"credits": {"cast": "nope"}}, MediaType.MOVIE) == []
    odd = {"aggregate_credits": {"cast": [{"name": "A", "roles": []}, 7, {"roles": [{}]}]}}
    assert billed_cast(odd, MediaType.TV) == []


def test_acquires_a_bounded_packet() -> None:
    web = FakeWeb()
    packet = _acquire(web=web)

    assert [doc.source_id for doc in packet.documents] == [
        "tmdb:tv:1396",
        "wikipedia:en:1001",
        "wikipedia:en:1002",
        "wikipedia:en:1003",
        "wikipedia:en:1004",
    ]
    assert [doc.metadata["role"] for doc in packet.documents] == [
        "metadata",
        "main",
        "characters",
        "character",
        "character",
    ]
    assert packet.retrieved_at == NOW
    assert len(web.sent_to(WIKIDATA_HOST)) == 1
    assert len(web.sent_to(WIKIPEDIA_HOST)) <= MAX_WIKI_REQUESTS


def test_documents_keep_their_provenance() -> None:
    packet = _acquire()
    tmdb_doc, main = packet.documents[0], packet.documents[1]

    assert tmdb_doc.source_type is SourceType.TMDB
    assert tmdb_doc.source_url == "https://www.themoviedb.org/tv/1396"
    assert tmdb_doc.metadata["wikidata_id"] == "Q1079"
    assert "Overview: A chemistry teacher" in tmdb_doc.text

    assert main.source_type is SourceType.WIKIPEDIA
    assert main.title == "Breaking Bad"
    assert main.source_url == "https://en.wikipedia.org/wiki/Breaking_Bad"
    assert main.retrieved_at == NOW
    assert main.metadata["pageid"] == 1001
    assert main.metadata["revid"] == 501001
    assert main.metadata["rev_timestamp"] == "2026-09-27T20:29:13Z"
    assert main.metadata["wikibase_item"] == "Q1079"
    assert main.metadata["license"] == "CC BY-SA 4.0"

    sources = {source.provider: source for source in packet.provenance.sources}
    assert sources[SourceType.TMDB].documents == 1
    assert sources[SourceType.WIKIPEDIA].documents == 4


def test_facts_are_canon_and_cite_their_source() -> None:
    packet = _acquire()
    assert len(packet.facts) == 26
    assert all(fact.classification is Classification.CANON for fact in packet.facts)
    assert all(fact.source_ids == ["tmdb:tv:1396"] for fact in packet.facts)
    assert all(fact.confidence == 1.0 for fact in packet.facts)
    triples = {(fact.subject, fact.predicate, fact.object) for fact in packet.facts}
    assert ("Breaking Bad", "created_by", "Vince Gilligan") in triples
    assert ("Breaking Bad", "genre", "Crime") in triples
    assert ("Walter White", "portrayed_by", "Bryan Cranston") in triples
    assert ("Saul Goodman", "episode_count", "43") in triples
    # The same facts are readable evidence inside the TMDB document.
    assert "- Walter White | portrayed_by | Bryan Cranston" in packet.documents[0].text


def test_misses_are_recorded_not_fatal() -> None:
    notes = _acquire().provenance.notes
    assert "no page found for 'Skyler White Breaking Bad'" in notes
    assert any("already included page" in note for note in notes)
    # A search hit about someone else is never accepted as canon.
    assert "rejected unrelated page 'Hank Williams' for 'Hank Schrader Breaking Bad'" in notes


def test_retrieval_reproduces_the_saved_packet() -> None:
    assert _acquire() == saved_packet()


def test_movie_acquisition() -> None:
    packet = _acquire("The Matrix")
    assert packet.universe.universe_id == "the_matrix_movie_603"
    assert [doc.source_id for doc in packet.documents] == ["tmdb:movie:603", "wikipedia:en:2001"]
    triples = {(fact.subject, fact.predicate, fact.object) for fact in packet.facts}
    assert ("The Matrix", "directed_by", "Lana Wachowski") in triples
    assert ("The Matrix", "release_date", "1999-03-31") in triples
    assert ("Morpheus", "portrayed_by", "Laurence Fishburne") in triples


def test_main_article_found_by_search_without_a_wikidata_id() -> None:
    details = fixture("tmdb_tv_1396.json")
    details["external_ids"] = {"imdb_id": "tt0903747", "wikidata_id": None}
    search = fixture("tmdb_search_breaking_bad.json")

    def tmdb(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json=search if "search" in request.url.path else details)

    web = FakeWeb(tmdb=tmdb)
    packet = _acquire(web=web)
    assert packet.documents[1].source_id == "wikipedia:en:1001"
    assert web.sent_to(WIKIDATA_HOST) == []
    assert any("matched by search" in note for note in packet.provenance.notes)


def test_no_main_article_means_no_packet() -> None:
    web = FakeWeb(wikipedia=lambda request: httpx.Response(200, json=wiki_page(None)))
    with pytest.raises(CanonAcquisitionError, match="Breaking Bad"):
        _acquire(web=web)


def test_main_article_provider_failure_is_reported() -> None:
    web = FakeWeb(wikipedia=lambda request: httpx.Response(200, text="<html>busy</html>"))
    with pytest.raises(ProviderError) as caught:
        _acquire(web=web)
    assert (caught.value.provider, caught.value.kind) == ("Wikipedia", "malformed")


def test_supporting_page_failure_stops_retrieval_but_keeps_the_packet() -> None:
    def wikipedia(request: httpx.Request) -> httpx.Response:
        if "gsrsearch" in request.url.params:
            return httpx.Response(429, text="slow down")
        return httpx.Response(200, json=wiki_page(request.url.params["titles"]))

    web = FakeWeb(wikipedia=wikipedia)
    packet = _acquire(web=web)
    assert [doc.source_id for doc in packet.documents] == ["tmdb:tv:1396", "wikipedia:en:1001"]
    assert "stopped retrieving supporting pages" in packet.provenance.notes[-1]
    # Main article, then one refused search and its single retry. Nothing after.
    assert len(web.sent_to(WIKIPEDIA_HOST)) == 3


def test_rate_limited_request_is_retried_exactly_once() -> None:
    calls = {"count": 0}

    def wikipedia(request: httpx.Request) -> httpx.Response:
        calls["count"] += 1
        if calls["count"] == 1:
            return httpx.Response(429, text="slow down")
        return FakeWeb._wikipedia(request)

    packet = _acquire(web=FakeWeb(wikipedia=wikipedia))
    assert len(packet.documents) == 5


def test_wikipedia_requests_are_identified_and_carry_no_key() -> None:
    web = FakeWeb()
    _acquire(web=web)
    for request in web.sent_to(WIKIPEDIA_HOST) + web.sent_to(WIKIDATA_HOST):
        assert request.headers["user-agent"] == USER_AGENT
        assert all(secret not in str(request.url) for secret in SECRET_VALUES)
        assert "authorization" not in request.headers
