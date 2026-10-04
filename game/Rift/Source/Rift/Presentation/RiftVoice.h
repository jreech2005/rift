// Helpers for playing voiced NPC lines: where a clip lives and how to read it. See docs/VOICE.md.

#pragma once

#include "CoreMinimal.h"

/** A decoded WAV clip. Pcm points into the bytes it was parsed from */
struct FRiftWavClip
{
	int32 SampleRate = 0;

	int32 NumChannels = 0;

	/** Signed 16 bit little endian samples, whole frames only */
	TArrayView<const uint8> Pcm;

	float GetDuration() const;
};

namespace RiftVoice
{
	/**
	 *  Turns the audio_url of a world event into a full URL on the backend that owns the WebSocket:
	 *  ws://host:port/ws and /audio/id give http://host:port/audio/id.
	 *  Only paths are accepted, so an event can never point the client at another host. Empty when unusable.
	 */
	FString ResolveAudioUrl(const FString& BackendUrl, const FString& AudioUrl);

	/** Reads an uncompressed 16 bit mono or stereo WAV file. False for anything else, MP3 included */
	bool ParseWav(TArrayView<const uint8> Bytes, FRiftWavClip& OutClip);
}
