// Helpers for playing voiced NPC lines: where a clip lives and how to read it. See docs/VOICE.md.

#include "RiftVoice.h"

namespace
{
	bool HasTag(TArrayView<const uint8> Bytes, int32 Offset, const ANSICHAR* Tag)
	{
		return Offset + 4 <= Bytes.Num() && FMemory::Memcmp(Bytes.GetData() + Offset, Tag, 4) == 0;
	}

	uint32 ReadLittleEndian(TArrayView<const uint8> Bytes, int32 Offset, int32 Size)
	{
		uint32 Value = 0;

		for (int32 Index = 0; Index < Size; ++Index)
		{
			Value |= static_cast<uint32>(Bytes[Offset + Index]) << (8 * Index);
		}

		return Value;
	}
}

float FRiftWavClip::GetDuration() const
{
	const int32 BytesPerSecond = SampleRate * NumChannels * 2;
	return BytesPerSecond > 0 ? static_cast<float>(Pcm.Num()) / BytesPerSecond : 0.0f;
}

FString RiftVoice::ResolveAudioUrl(const FString& BackendUrl, const FString& AudioUrl)
{
	// a path on the backend, never a full URL or a //host reference
	if (!AudioUrl.StartsWith(TEXT("/")) || AudioUrl.StartsWith(TEXT("//")))
	{
		return FString();
	}

	FString Scheme;
	FString Rest;

	if (!BackendUrl.Split(TEXT("://"), &Scheme, &Rest))
	{
		return FString();
	}

	if (Scheme.Equals(TEXT("ws"), ESearchCase::IgnoreCase))
	{
		Scheme = TEXT("http");
	}
	else if (Scheme.Equals(TEXT("wss"), ESearchCase::IgnoreCase))
	{
		Scheme = TEXT("https");
	}
	else if (!Scheme.Equals(TEXT("http"), ESearchCase::IgnoreCase) && !Scheme.Equals(TEXT("https"), ESearchCase::IgnoreCase))
	{
		return FString();
	}

	int32 PathStart = INDEX_NONE;
	const FString Authority = Rest.FindChar(TEXT('/'), PathStart) ? Rest.Left(PathStart) : Rest;

	return Authority.IsEmpty() ? FString() : FString::Printf(TEXT("%s://%s%s"), *Scheme, *Authority, *AudioUrl);
}

bool RiftVoice::ParseWav(TArrayView<const uint8> Bytes, FRiftWavClip& OutClip)
{
	if (!HasTag(Bytes, 0, "RIFF") || !HasTag(Bytes, 8, "WAVE"))
	{
		return false;
	}

	FRiftWavClip Clip;
	bool bHasFormat = false;

	// chunks: 4 byte tag, 4 byte size, data padded to an even length
	int64 Offset = 12;

	while (Offset + 8 <= Bytes.Num())
	{
		const int32 Header = static_cast<int32>(Offset);
		const int64 DataStart = Offset + 8;
		const int64 ChunkSize = FMath::Min<int64>(ReadLittleEndian(Bytes, Header + 4, 4), Bytes.Num() - DataStart);

		if (HasTag(Bytes, Header, "fmt "))
		{
			if (ChunkSize < 16)
			{
				return false;
			}

			const int32 Format = static_cast<int32>(DataStart);
			const uint32 Encoding = ReadLittleEndian(Bytes, Format, 2);
			const uint32 BitsPerSample = ReadLittleEndian(Bytes, Format + 14, 2);

			Clip.NumChannels = ReadLittleEndian(Bytes, Format + 2, 2);
			Clip.SampleRate = ReadLittleEndian(Bytes, Format + 4, 4);

			// plain PCM, 16 bit, mono or stereo at a sane rate
			if (Encoding != 1 || BitsPerSample != 16 || Clip.NumChannels < 1 || Clip.NumChannels > 2 ||
				Clip.SampleRate < 8000 || Clip.SampleRate > 96000)
			{
				return false;
			}

			bHasFormat = true;
		}
		else if (HasTag(Bytes, Header, "data"))
		{
			if (!bHasFormat)
			{
				return false;
			}

			const int32 FrameSize = Clip.NumChannels * 2;
			const int32 PcmSize = static_cast<int32>(ChunkSize) / FrameSize * FrameSize;

			if (PcmSize == 0)
			{
				return false;
			}

			Clip.Pcm = Bytes.Slice(static_cast<int32>(DataStart), PcmSize);
			OutClip = Clip;
			return true;
		}

		Offset = DataStart + ChunkSize + (ChunkSize & 1);
	}

	return false;
}
