// SPDX-License-Identifier: GPL-2.0

#include <linux/pm_runtime.h>

__rust_helper void rust_helper_pm_runtime_get_noresume(struct device *dev)
{
	pm_runtime_get_noresume(dev);
}

__rust_helper void rust_helper_pm_runtime_put_noidle(struct device *dev)
{
	pm_runtime_put_noidle(dev);
}

__rust_helper void rust_helper_pm_runtime_mark_last_busy(struct device *dev)
{
	pm_runtime_mark_last_busy(dev) ;
}

__rust_helper bool rust_helper_pm_runtime_active(struct device *dev)
{
	return pm_runtime_active(dev);
}

__rust_helper bool rust_helper_pm_runtime_suspended(struct device *dev)
{
	return pm_runtime_suspended(dev);
}

__rust_helper void rust_helper_pm_suspend_ignore_children(struct device *dev,
							  bool enable)
{
	pm_suspend_ignore_children(dev, enable);
}

__rust_helper int rust_helper_pm_runtime_set_active(struct device *dev)
{
	return pm_runtime_set_active(dev);
}

__rust_helper int rust_helper_pm_runtime_set_suspended(struct device *dev)
{
	return pm_runtime_set_suspended(dev);
}
