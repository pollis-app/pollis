-- No client IP address may be readable anywhere we store data, admins
-- included. Until this release, desktop clients registered each device as
-- "{hostname} ({os})", and a cloud, corporate or ISP hostname routinely embeds
-- the machine's address (ip-10-0-0-12, c-73-162-1-2.hsd1.ca.comcast.net). That
-- name sits in user_device.device_name and was copied into the account's
-- security log as security_event.metadata = 'name=<device_name>'.
--
-- Clients now register an OS label ("macOS desktop") and the DS redacts any
-- IP-shaped token on write (pollis-delivery/src/util.rs `redact_ip_literals`).
-- This scrubs the rows written before that.
--
-- SQLite has no regex, so "IP-shaped" is GLOB: four digit groups of 1-3 digits
-- joined by '.', '-' or '_' (a leading/trailing `*` absorbs longer outer
-- groups), or an IPv6 shape ('::', or three hex-flanked colons). It errs toward
-- scrubbing: a false positive only replaces a device's name with its OS label.
-- A matching name is replaced WHOLE by that label (taken from the legacy
-- "(os)" suffix) rather than partially edited, so no fragment of the hostname
-- survives. Only 'name=' metadata is client-copied device text; every other
-- security_event row is DS-authored and untouched.
--
-- Data-only and idempotent: no schema change, and a scrubbed value never
-- matches again.

UPDATE user_device
SET device_name = CASE
        WHEN device_name LIKE '%(macos)' THEN 'macOS desktop'
        WHEN device_name LIKE '%(windows)' THEN 'Windows desktop'
        WHEN device_name LIKE '%(linux)' THEN 'Linux desktop'
        WHEN device_name LIKE '%(ios)' THEN 'iOS device'
        WHEN device_name LIKE '%(android)' THEN 'Android device'
        ELSE 'device'
    END
WHERE device_name IS NOT NULL AND (
    device_name GLOB '*[0-9].[0-9].[0-9].[0-9]*'
    OR device_name GLOB '*[0-9].[0-9].[0-9][0-9].[0-9]*'
    OR device_name GLOB '*[0-9].[0-9].[0-9][0-9][0-9].[0-9]*'
    OR device_name GLOB '*[0-9].[0-9][0-9].[0-9].[0-9]*'
    OR device_name GLOB '*[0-9].[0-9][0-9].[0-9][0-9].[0-9]*'
    OR device_name GLOB '*[0-9].[0-9][0-9].[0-9][0-9][0-9].[0-9]*'
    OR device_name GLOB '*[0-9].[0-9][0-9][0-9].[0-9].[0-9]*'
    OR device_name GLOB '*[0-9].[0-9][0-9][0-9].[0-9][0-9].[0-9]*'
    OR device_name GLOB '*[0-9].[0-9][0-9][0-9].[0-9][0-9][0-9].[0-9]*'
    OR device_name GLOB '*[0-9]-[0-9]-[0-9]-[0-9]*'
    OR device_name GLOB '*[0-9]-[0-9]-[0-9][0-9]-[0-9]*'
    OR device_name GLOB '*[0-9]-[0-9]-[0-9][0-9][0-9]-[0-9]*'
    OR device_name GLOB '*[0-9]-[0-9][0-9]-[0-9]-[0-9]*'
    OR device_name GLOB '*[0-9]-[0-9][0-9]-[0-9][0-9]-[0-9]*'
    OR device_name GLOB '*[0-9]-[0-9][0-9]-[0-9][0-9][0-9]-[0-9]*'
    OR device_name GLOB '*[0-9]-[0-9][0-9][0-9]-[0-9]-[0-9]*'
    OR device_name GLOB '*[0-9]-[0-9][0-9][0-9]-[0-9][0-9]-[0-9]*'
    OR device_name GLOB '*[0-9]-[0-9][0-9][0-9]-[0-9][0-9][0-9]-[0-9]*'
    OR device_name GLOB '*[0-9]_[0-9]_[0-9]_[0-9]*'
    OR device_name GLOB '*[0-9]_[0-9]_[0-9][0-9]_[0-9]*'
    OR device_name GLOB '*[0-9]_[0-9]_[0-9][0-9][0-9]_[0-9]*'
    OR device_name GLOB '*[0-9]_[0-9][0-9]_[0-9]_[0-9]*'
    OR device_name GLOB '*[0-9]_[0-9][0-9]_[0-9][0-9]_[0-9]*'
    OR device_name GLOB '*[0-9]_[0-9][0-9]_[0-9][0-9][0-9]_[0-9]*'
    OR device_name GLOB '*[0-9]_[0-9][0-9][0-9]_[0-9]_[0-9]*'
    OR device_name GLOB '*[0-9]_[0-9][0-9][0-9]_[0-9][0-9]_[0-9]*'
    OR device_name GLOB '*[0-9]_[0-9][0-9][0-9]_[0-9][0-9][0-9]_[0-9]*'
    OR device_name LIKE '%::%'
    OR device_name GLOB '*[0-9a-fA-F]:[0-9a-fA-F]*:[0-9a-fA-F]*:[0-9a-fA-F]*'
);

UPDATE security_event
SET metadata = 'name=' || CASE
        WHEN metadata LIKE '%(macos)' THEN 'macOS desktop'
        WHEN metadata LIKE '%(windows)' THEN 'Windows desktop'
        WHEN metadata LIKE '%(linux)' THEN 'Linux desktop'
        WHEN metadata LIKE '%(ios)' THEN 'iOS device'
        WHEN metadata LIKE '%(android)' THEN 'Android device'
        ELSE 'device'
    END
WHERE metadata LIKE 'name=%' AND (
    metadata GLOB '*[0-9].[0-9].[0-9].[0-9]*'
    OR metadata GLOB '*[0-9].[0-9].[0-9][0-9].[0-9]*'
    OR metadata GLOB '*[0-9].[0-9].[0-9][0-9][0-9].[0-9]*'
    OR metadata GLOB '*[0-9].[0-9][0-9].[0-9].[0-9]*'
    OR metadata GLOB '*[0-9].[0-9][0-9].[0-9][0-9].[0-9]*'
    OR metadata GLOB '*[0-9].[0-9][0-9].[0-9][0-9][0-9].[0-9]*'
    OR metadata GLOB '*[0-9].[0-9][0-9][0-9].[0-9].[0-9]*'
    OR metadata GLOB '*[0-9].[0-9][0-9][0-9].[0-9][0-9].[0-9]*'
    OR metadata GLOB '*[0-9].[0-9][0-9][0-9].[0-9][0-9][0-9].[0-9]*'
    OR metadata GLOB '*[0-9]-[0-9]-[0-9]-[0-9]*'
    OR metadata GLOB '*[0-9]-[0-9]-[0-9][0-9]-[0-9]*'
    OR metadata GLOB '*[0-9]-[0-9]-[0-9][0-9][0-9]-[0-9]*'
    OR metadata GLOB '*[0-9]-[0-9][0-9]-[0-9]-[0-9]*'
    OR metadata GLOB '*[0-9]-[0-9][0-9]-[0-9][0-9]-[0-9]*'
    OR metadata GLOB '*[0-9]-[0-9][0-9]-[0-9][0-9][0-9]-[0-9]*'
    OR metadata GLOB '*[0-9]-[0-9][0-9][0-9]-[0-9]-[0-9]*'
    OR metadata GLOB '*[0-9]-[0-9][0-9][0-9]-[0-9][0-9]-[0-9]*'
    OR metadata GLOB '*[0-9]-[0-9][0-9][0-9]-[0-9][0-9][0-9]-[0-9]*'
    OR metadata GLOB '*[0-9]_[0-9]_[0-9]_[0-9]*'
    OR metadata GLOB '*[0-9]_[0-9]_[0-9][0-9]_[0-9]*'
    OR metadata GLOB '*[0-9]_[0-9]_[0-9][0-9][0-9]_[0-9]*'
    OR metadata GLOB '*[0-9]_[0-9][0-9]_[0-9]_[0-9]*'
    OR metadata GLOB '*[0-9]_[0-9][0-9]_[0-9][0-9]_[0-9]*'
    OR metadata GLOB '*[0-9]_[0-9][0-9]_[0-9][0-9][0-9]_[0-9]*'
    OR metadata GLOB '*[0-9]_[0-9][0-9][0-9]_[0-9]_[0-9]*'
    OR metadata GLOB '*[0-9]_[0-9][0-9][0-9]_[0-9][0-9]_[0-9]*'
    OR metadata GLOB '*[0-9]_[0-9][0-9][0-9]_[0-9][0-9][0-9]_[0-9]*'
    OR metadata LIKE '%::%'
    OR metadata GLOB '*[0-9a-fA-F]:[0-9a-fA-F]*:[0-9a-fA-F]*:[0-9a-fA-F]*'
);
