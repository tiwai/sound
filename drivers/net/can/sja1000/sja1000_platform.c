// SPDX-License-Identifier: GPL-2.0-only
/*
 * Copyright (C) 2005 Sascha Hauer, Pengutronix
 * Copyright (C) 2007 Wolfgang Grandegger <wg@grandegger.com>
 */

#include <linux/kernel.h>
#include <linux/module.h>
#include <linux/interrupt.h>
#include <linux/netdevice.h>
#include <linux/delay.h>
#include <linux/pci.h>
#include <linux/platform_device.h>
#include <linux/irq.h>
#include <linux/can/dev.h>
#include <linux/can/platform/sja1000.h>
#include <linux/clk.h>
#include <linux/io.h>
#include <linux/of.h>

#include "sja1000.h"

#define DRV_NAME "sja1000_platform"
#define SP_CAN_CLOCK  (16000000 / 2)

MODULE_AUTHOR("Sascha Hauer <s.hauer@pengutronix.de>");
MODULE_AUTHOR("Wolfgang Grandegger <wg@grandegger.com>");
MODULE_DESCRIPTION("Socket-CAN driver for SJA1000 on the platform bus");
MODULE_ALIAS("platform:" DRV_NAME);
MODULE_LICENSE("GPL v2");

struct sja1000_of_data {
	size_t  priv_sz;
	void    (*init)(struct sja1000_priv *priv, struct device_node *of);
};

struct technologic_priv {
	spinlock_t      io_lock;
};

static u8 sp_read_reg8(const struct sja1000_priv *priv, int reg)
{
	return ioread8(priv->reg_base + reg);
}

static void sp_write_reg8(const struct sja1000_priv *priv, int reg, u8 val)
{
	iowrite8(val, priv->reg_base + reg);
}

static u8 sp_read_reg16(const struct sja1000_priv *priv, int reg)
{
	return ioread8(priv->reg_base + reg * 2);
}

static void sp_write_reg16(const struct sja1000_priv *priv, int reg, u8 val)
{
	iowrite8(val, priv->reg_base + reg * 2);
}

static u8 sp_read_reg32(const struct sja1000_priv *priv, int reg)
{
	return ioread8(priv->reg_base + reg * 4);
}

static void sp_write_reg32(const struct sja1000_priv *priv, int reg, u8 val)
{
	iowrite8(val, priv->reg_base + reg * 4);
}

static u8 sp_technologic_read_reg16(const struct sja1000_priv *priv, int reg)
{
	struct technologic_priv *tp = priv->priv;
	unsigned long flags;
	u8 val;

	spin_lock_irqsave(&tp->io_lock, flags);
	iowrite16(reg, priv->reg_base + 0);
	val = ioread16(priv->reg_base + 2);
	spin_unlock_irqrestore(&tp->io_lock, flags);

	return val;
}

static void sp_technologic_write_reg16(const struct sja1000_priv *priv,
				       int reg, u8 val)
{
	struct technologic_priv *tp = priv->priv;
	unsigned long flags;

	spin_lock_irqsave(&tp->io_lock, flags);
	iowrite16(reg, priv->reg_base + 0);
	iowrite16(val, priv->reg_base + 2);
	spin_unlock_irqrestore(&tp->io_lock, flags);
}

static void sp_technologic_init(struct sja1000_priv *priv, struct device_node *of)
{
	struct technologic_priv *tp = priv->priv;

	priv->read_reg = sp_technologic_read_reg16;
	priv->write_reg = sp_technologic_write_reg16;
	spin_lock_init(&tp->io_lock);
}

static void sp_rzn1_init(struct sja1000_priv *priv, struct device_node *of)
{
	priv->flags = SJA1000_QUIRK_NO_CDR_REG | SJA1000_QUIRK_RESET_ON_OVERRUN;
}

static void sp_populate(struct sja1000_priv *priv,
			struct sja1000_platform_data *pdata,
			unsigned long resource_mem_flags)
{
	/* The CAN clock frequency is half the oscillator clock frequency */
	priv->can.clock.freq = pdata->osc_freq / 2;
	priv->ocr = pdata->ocr;
	priv->cdr = pdata->cdr;

	switch (resource_mem_flags & IORESOURCE_MEM_TYPE_MASK) {
	case IORESOURCE_MEM_32BIT:
		priv->read_reg = sp_read_reg32;
		priv->write_reg = sp_write_reg32;
		break;
	case IORESOURCE_MEM_16BIT:
		priv->read_reg = sp_read_reg16;
		priv->write_reg = sp_write_reg16;
		break;
	case IORESOURCE_MEM_8BIT:
	default:
		priv->read_reg = sp_read_reg8;
		priv->write_reg = sp_write_reg8;
		break;
	}
}

static void sp_populate_of(struct sja1000_priv *priv, struct device_node *of)
{
	int err;
	u32 prop;

	err = of_property_read_u32(of, "reg-io-width", &prop);
	if (err)
		prop = 1; /* 8 bit is default */

	switch (prop) {
	case 4:
		priv->read_reg = sp_read_reg32;
		priv->write_reg = sp_write_reg32;
		break;
	case 2:
		priv->read_reg = sp_read_reg16;
		priv->write_reg = sp_write_reg16;
		break;
	case 1:
	default:
		priv->read_reg = sp_read_reg8;
		priv->write_reg = sp_write_reg8;
	}

	if (!priv->can.clock.freq) {
		err = of_property_read_u32(of, "nxp,external-clock-frequency", &prop);
		if (!err)
			priv->can.clock.freq = prop / 2;
		else
			priv->can.clock.freq = SP_CAN_CLOCK; /* default */
	}

	err = of_property_read_u32(of, "nxp,tx-output-mode", &prop);
	if (!err)
		priv->ocr |= prop & OCR_MODE_MASK;
	else
		priv->ocr |= OCR_MODE_NORMAL; /* default */

	err = of_property_read_u32(of, "nxp,tx-output-config", &prop);
	if (!err)
		priv->ocr |= (prop << OCR_TX_SHIFT) & OCR_TX_MASK;
	else
		priv->ocr |= OCR_TX0_PULLDOWN; /* default */

	err = of_property_read_u32(of, "nxp,clock-out-frequency", &prop);
	if (!err && prop) {
		u32 divider = priv->can.clock.freq * 2 / prop;

		if (divider > 1)
			priv->cdr |= divider / 2 - 1;
		else
			priv->cdr |= CDR_CLKOUT_MASK;
	} else {
		priv->cdr |= CDR_CLK_OFF; /* default */
	}

	if (!of_property_read_bool(of, "nxp,no-comparator-bypass"))
		priv->cdr |= CDR_CBP; /* default */
}

static struct sja1000_of_data technologic_data = {
	.priv_sz = sizeof(struct technologic_priv),
	.init = sp_technologic_init,
};

static struct sja1000_of_data renesas_data = {
	.init = sp_rzn1_init,
};

static const struct of_device_id sp_of_table[] = {
	{ .compatible = "nxp,sja1000", .data = NULL, },
	{ .compatible = "renesas,rzn1-sja1000", .data = &renesas_data, },
	{ .compatible = "technologic,sja1000", .data = &technologic_data, },
	{ /* sentinel */ },
};
MODULE_DEVICE_TABLE(of, sp_of_table);

static int sp_probe(struct platform_device *pdev)
{
	int err, irq = 0;
	void __iomem *addr;
	struct net_device *dev;
	struct sja1000_priv *priv;
	struct resource *res_mem, *res_irq = NULL;
	struct sja1000_platform_data *pdata;
	struct device_node *of = pdev->dev.of_node;
	const struct sja1000_of_data *of_data = NULL;
	size_t priv_sz = 0;
	struct clk *clk;

	pdata = dev_get_platdata(&pdev->dev);
	if (!pdata && !of) {
		dev_err(&pdev->dev, "No platform data provided!\n");
		return -ENODEV;
	}

	addr = devm_platform_get_and_ioremap_resource(pdev, 0, &res_mem);
	if (IS_ERR(addr))
		return PTR_ERR(addr);

	if (of) {
		irq = platform_get_irq(pdev, 0);
		if (irq < 0)
			return irq;

		clk = devm_clk_get_optional_enabled(&pdev->dev, NULL);
		if (IS_ERR(clk))
			return dev_err_probe(&pdev->dev, PTR_ERR(clk),
					     "CAN clk operation failed");
	} else {
		res_irq = platform_get_resource(pdev, IORESOURCE_IRQ, 0);
		if (!res_irq)
			return -ENODEV;
	}

	of_data = device_get_match_data(&pdev->dev);
	if (of_data)
		priv_sz = of_data->priv_sz;

	dev = alloc_sja1000dev(priv_sz);
	if (!dev)
		return -ENOMEM;
	priv = netdev_priv(dev);

	if (res_irq) {
		irq = res_irq->start;
		priv->irq_flags = res_irq->flags & IRQF_TRIGGER_MASK;
		if (res_irq->flags & IORESOURCE_IRQ_SHAREABLE)
			priv->irq_flags |= IRQF_SHARED;
	} else {
		priv->irq_flags = IRQF_SHARED;
	}

	if (priv->flags & SJA1000_QUIRK_RESET_ON_OVERRUN)
		priv->irq_flags |= IRQF_ONESHOT;

	dev->irq = irq;
	priv->reg_base = addr;

	if (of) {
		if (clk) {
			priv->can.clock.freq  = clk_get_rate(clk) / 2;
			if (!priv->can.clock.freq) {
				err = -EINVAL;
				dev_err(&pdev->dev, "Zero CAN clk rate");
				goto exit_free;
			}
		}

		sp_populate_of(priv, of);

		if (of_data && of_data->init)
			of_data->init(priv, of);
	} else {
		sp_populate(priv, pdata, res_mem->flags);
	}

	platform_set_drvdata(pdev, dev);
	SET_NETDEV_DEV(dev, &pdev->dev);

	err = register_sja1000dev(dev);
	if (err) {
		dev_err(&pdev->dev, "registering %s failed (err=%d)\n",
			DRV_NAME, err);
		goto exit_free;
	}

	dev_info(&pdev->dev, "%s device registered (reg_base=%p, irq=%d)\n",
		 DRV_NAME, priv->reg_base, dev->irq);
	return 0;

 exit_free:
	free_sja1000dev(dev);
	return err;
}

static void sp_remove(struct platform_device *pdev)
{
	struct net_device *dev = platform_get_drvdata(pdev);

	unregister_sja1000dev(dev);
	free_sja1000dev(dev);
}

static struct platform_driver sp_driver = {
	.probe = sp_probe,
	.remove = sp_remove,
	.driver = {
		.name = DRV_NAME,
		.of_match_table = sp_of_table,
	},
};

module_platform_driver(sp_driver);
