// SPDX-License-Identifier: GPL-2.0

#include <sound/core.h>
#include <sound/pcm.h>

/* DMA allocation helper */
__rust_helper struct snd_dma_buffer *rust_helper_snd_devm_alloc_pages(
        struct device *dev, int type, size_t size)
{
        return snd_devm_alloc_pages(dev, type, size);
}

/* hw_params field accessors (inline in C, need wrappers for Rust) */
__rust_helper unsigned int rust_helper_params_rate(const struct snd_pcm_hw_params *p)
{
	return params_rate(p);
}

__rust_helper unsigned int rust_helper_params_channels(const struct snd_pcm_hw_params *p)
{
	return params_channels(p);
}

__rust_helper int rust_helper_params_format(const struct snd_pcm_hw_params *p)
{
	/* params_format is not present in all kernels; read the mask directly. */
	const struct snd_mask *fmt = hw_param_mask_c(p, SNDRV_PCM_HW_PARAM_FORMAT);
	int i;

	for (i = 0; i < (int)(sizeof(fmt->bits) * 8); i++) {
		if (fmt->bits[i / 32] & (1U << (i % 32)))
			return i;
	}
	return -1; /* no format */
}

__rust_helper unsigned int rust_helper_params_period_size(const struct snd_pcm_hw_params *p)
{
	return params_period_size(p);
}

__rust_helper unsigned int rust_helper_params_buffer_size(const struct snd_pcm_hw_params *p)
{
	return params_buffer_size(p);
}

__rust_helper unsigned int rust_helper_params_periods(const struct snd_pcm_hw_params *p)
{
	return params_periods(p);
}

/* hw_param_interval / hw_param_mask are macros; wrap them for Rust */
__rust_helper struct snd_interval *rust_helper_hw_param_interval(
	struct snd_pcm_hw_params *p, int n)
{
	return hw_param_interval(p, n);
}

__rust_helper struct snd_mask *rust_helper_hw_param_mask(
	struct snd_pcm_hw_params *p, int n)
{
	return hw_param_mask(p, n);
}

/*
 * snd_pcm_hw_rule_add is variadic; provide a fixed 4-dep wrapper.
 * Pass -1 for unused dep slots.
 */
__rust_helper int rust_helper_snd_pcm_hw_rule_add(
	struct snd_pcm_runtime *runtime, unsigned int cond, int var,
	snd_pcm_hw_rule_func_t func, void *private,
	int dep0, int dep1, int dep2, int dep3)
{
	return snd_pcm_hw_rule_add(runtime, cond, var, func, private,
				   dep0, dep1, dep2, dep3, -1);
}
