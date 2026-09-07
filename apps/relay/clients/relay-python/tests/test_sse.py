import pytest

from relay_sdk.errors import RelayProtocolError
from relay_sdk.sse import SseDecoder


def test_split_frames_and_done_are_incremental() -> None:
    decoder = SseDecoder(max_event_bytes=128)
    assert decoder.feed(b'data: {"delta":') == []
    assert decoder.feed(b'"hello"}\r\n\r\ndata: [DONE]\n\n') == [{"delta": "hello"}]
    assert decoder.done
    assert decoder.finish() == []


def test_event_size_and_json_are_bounded() -> None:
    decoder = SseDecoder(max_event_bytes=8)
    with pytest.raises(RelayProtocolError, match="exceeded"):
        decoder.feed(b"data: 123456789")

    decoder = SseDecoder()
    with pytest.raises(RelayProtocolError, match="valid JSON"):
        decoder.feed(b"data: nope\n\n")
