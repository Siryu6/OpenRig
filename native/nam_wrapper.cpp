// C interface to NeuralAudio for the Rust side (src/plugins/nam.rs).
//
// Unlike the C API shipped with NeuralAudio, paths are UTF-8 and no C++
// exception crosses the FFI boundary: loading errors are returned as text.

#include <cstring>
#include <exception>
#include <filesystem>
#include <string>

#include "NeuralAudio/NeuralModel.h"
#include "NeuralAudio/CompositeModel.h"

namespace
{
	void copy_error(const std::string& message, char* error, size_t error_len)
	{
		if (error == nullptr || error_len == 0)
			return;
		size_t len = std::min(message.size(), error_len - 1);
		std::memcpy(error, message.data(), len);
		error[len] = '\0';
	}
}

extern "C"
{
	// Load a model (.nam, or keras .json/.aidax). Returns null on failure,
	// with the reason in `error`. Not real-time safe.
	NeuralAudio::NeuralModel* pb_nam_load(const char* path, int sample_rate, int max_block,
		char* error, size_t error_len)
	{
		try
		{
			NeuralAudio::NeuralModelLoader loader;
			loader.SetDefaultMaxAudioBufferSize(max_block);
			loader.SetExternalSampleRate(sample_rate);

			std::filesystem::path model_path(reinterpret_cast<const char8_t*>(path));
			NeuralAudio::NeuralModel* model = loader.CreateFromFile(model_path);
			if (model == nullptr)
				copy_error("unsupported or missing model file", error, error_len);
			return model;
		}
		catch (const std::exception& e)
		{
			copy_error(e.what(), error, error_len);
		}
		catch (...)
		{
			copy_error("unknown error", error, error_len);
		}
		return nullptr;
	}

	void pb_nam_free(NeuralAudio::NeuralModel* model)
	{
		delete model;
	}

	// `frames` must not exceed the max_block given at load time
	void pb_nam_process(NeuralAudio::NeuralModel* model, const float* input, float* output,
		size_t frames)
	{
		model->Process(input, output, frames);
	}

	// Sample rate the model was trained at (-1 if unknown)
	float pb_nam_sample_rate(NeuralAudio::NeuralModel* model)
	{
		return model->GetSampleRate();
	}

	// Gains (dB) bringing the input to the level the model expects and the
	// output to a standard loudness, from the model metadata (0 if unknown)
	float pb_nam_input_adjustment(NeuralAudio::NeuralModel* model)
	{
		return model->GetRecommendedInputDBAdjustment();
	}

	float pb_nam_output_adjustment(NeuralAudio::NeuralModel* model)
	{
		return model->GetRecommendedOutputDBAdjustment();
	}

	// True for a packed A2 file (SlimmableContainer): it holds the capture
	// in several sizes, all loaded and prewarmed, so the quality can change
	// while playing. A single slimmable WaveNet rebuilds itself instead:
	// not real-time safe, left at full quality.
	bool pb_nam_has_quality(NeuralAudio::NeuralModel* model)
	{
		return dynamic_cast<NeuralAudio::ScalableCompositeModel*>(model) != nullptr;
	}

	// 0 (lightest) to 1 (full). Real-time safe on a model with
	// pb_nam_has_quality: it only switches the size used.
	void pb_nam_set_quality(NeuralAudio::NeuralModel* model, float quality)
	{
		model->SetQualityScaleFactor(quality);
	}
}
