// SPDX-License-Identifier: GPL-2.0
/*
 * soc-apci-intel-glk-match.c - tables and support for GLK ACPI enumeration.
 *
 * Copyright (c) 2018, Intel Corporation.
 *
 */

#include <sound/soc-acpi.h>
#include <sound/soc-acpi-intel-match.h>

struct snd_soc_acpi_mach snd_soc_acpi_intel_glk_machines[] = {
	{
		.id = "INT343A",
		.drv_name = "glk_alc298s_i2s",
		.fw_filename = "intel/dsp_fw_glk.bin",
		.sof_fw_filename = "intel/sof-glk.ri",
		.sof_tplg_filename = "intel/sof-glk.tplg",
		.asoc_plat_name = "0000:00:0e.0",
	},
	{
		.id = "DLGS7219",
		.drv_name = "bxt_da7219_max98357a_i2s",
		.sof_fw_filename = "intel/sof-glk.ri",
		.sof_tplg_filename = "intel/sof-glk.tplg",
		.asoc_plat_name = "0000:00:0e.0",
	},
	{},
};
EXPORT_SYMBOL_GPL(snd_soc_acpi_intel_glk_machines);

MODULE_LICENSE("GPL v2");
MODULE_DESCRIPTION("Intel Common ACPI Match module");
