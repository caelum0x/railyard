# Aeonrift Champion Splash — Style LoRA

Flux-dev style LoRA trained to reproduce the locked "League-painterly" champion
splash look (dynamic action, painterly-photoreal faces, moody rim-lit cinematic
lighting) from the curated Aeonrift anchor set.

## How to use

- **Model version (runnable, flux-dev + LoRA baked in):**
  `caelum0x/aeon-style-lora:433a51a0e3f751b24f29f4c93b140f2e484e91723abd4a26a80c7576bb3e4642`
- **Trigger word:** `AEONSPLASH` — put it at the start of every prompt, then describe the
  subject, then the style tail (painterly semi-realistic, dynamic full-body action pose,
  low dramatic angle, moody atmosphere, strong rim lighting, volumetric fog, cinematic grade).
- **Recommended inference:** `aspect_ratio: "4:5"` (matches the 896×1088 anchors),
  `lora_scale: 1.0`, `guidance_scale: 3`, `num_inference_steps: 28`, `model: dev`.

## Training

- **Trainer:** `ostris/flux-dev-lora-trainer`
  (version `26dce37af90b9d997eeb970d92e47de3064d46c300504ae376c75bef6a9022d2`, H100).
- **Training job id:** `ywga6j429xrmt0d07gg8gjz7zm`
- **Training set:** 22 curated anchors (`train/`), balanced across all 8 factions.
- **Captions:** uniform / identical on every image — a shared trigger token + style
  descriptor only (NO per-champion subject captions), so the LoRA learns STYLE, not subjects.
- **Hyperparameters:** steps 1200, lora_rank 16, learning_rate 4e-4, batch_size 1,
  resolution `512,768,1024`, caption_dropout_rate 0.05, autocaption off.
- **Final loss:** ~0.3–0.4 (started ~0.53).

## Weights

- Raw LoRA weights tarball is downloaded to `weights/trained_model.tar` (gitignored — large).
- Original Replicate delivery URL (ephemeral):
  `https://replicate.delivery/xezq/AI3k8eXUZsVBFSp1aQ5BhSCGnAc9QjskM4ei8KzJiPstOBHXA/trained_model.tar`
- Canonical source of truth is the Replicate model version id above.
