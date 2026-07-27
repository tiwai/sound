// SPDX-License-Identifier: GPL-2.0

#include <sound/core.h>
#include <sound/pcm.h>

/* DMA allocation helper */
__rust_helper struct snd_dma_buffer *rust_helper_snd_devm_alloc_pages(
        struct device *dev, int type, size_t size)
{
        return snd_devm_alloc_pages(dev, type, size);
}
