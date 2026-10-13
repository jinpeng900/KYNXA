use std::fs::File;

#[derive(PartialEq, Eq)]
pub struct HandleIdentity {
    pub volume: u64,
    pub identity: u64,
    pub links: u64,
    pub is_link: bool,
}

/// Inspect the opened object, so all service domains use the same stable link and file identity rules.
/// 检查已打开的文件对象，各服务领域共用相同的稳定链接及文件身份规则。
pub fn handle_identity(file: &File) -> std::io::Result<HandleIdentity> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        #[derive(Default)]
        struct HandleInformation {
            attributes: u32,
            creation_low: u32,
            creation_high: u32,
            access_low: u32,
            access_high: u32,
            write_low: u32,
            write_high: u32,
            volume: u32,
            size_high: u32,
            size_low: u32,
            links: u32,
            index_high: u32,
            index_low: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandle(
                handle: *mut std::ffi::c_void,
                information: *mut HandleInformation,
            ) -> i32;
        }
        let mut information = HandleInformation::default();
        // The exact BY_HANDLE_FILE_INFORMATION buffer remains live throughout this borrowed handle call.
        // 精确匹配 BY_HANDLE_FILE_INFORMATION 的缓冲区在借用句柄调用期间始终有效。
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut information) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(HandleIdentity {
            volume: information.volume as u64,
            identity: (information.index_high as u64) << 32 | information.index_low as u64,
            links: information.links as u64,
            is_link: information.attributes & 0x400 != 0,
        })
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(HandleIdentity {
            volume: metadata.dev(),
            identity: metadata.ino(),
            links: metadata.nlink(),
            is_link: metadata.file_type().is_symlink(),
        })
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = file;
        Ok(HandleIdentity {
            volume: 0,
            identity: 0,
            links: 1,
            is_link: false,
        })
    }
}

/// A Windows change stamp detects in-place edits even when the last-write time is restored.
/// Windows 变更时间可识别原地修改，即使调用方恢复了最后写入时间；不支持时继续核对完整字节。
pub fn change_stamp(file: &File) -> std::io::Result<Option<i64>> {
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        #[repr(C)]
        #[derive(Default)]
        struct BasicInformation {
            creation_time: i64,
            access_time: i64,
            write_time: i64,
            change_time: i64,
            attributes: u32,
        }
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetFileInformationByHandleEx(
                handle: *mut std::ffi::c_void,
                class: u32,
                information: *mut std::ffi::c_void,
                size: u32,
            ) -> i32;
            fn GetVolumeInformationByHandleW(
                handle: *mut std::ffi::c_void,
                volume_name: *mut u16,
                volume_name_size: u32,
                serial_number: *mut u32,
                maximum_component_length: *mut u32,
                flags: *mut u32,
                filesystem_name: *mut u16,
                filesystem_name_size: u32,
            ) -> i32;
        }
        let mut filesystem_name = [0u16; 16];
        // Coarse timestamp filesystems cannot prove an unchanged source; they retain full hashing.
        // 粗粒度时间戳文件系统不能证明来源未变，继续采用完整哈希，不把时间相等当成字节相等。
        if unsafe {
            GetVolumeInformationByHandleW(
                file.as_raw_handle(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                filesystem_name.as_mut_ptr(),
                filesystem_name.len() as u32,
            )
        } == 0
        {
            return Ok(None);
        }
        let filesystem = String::from_utf16_lossy(&filesystem_name)
            .trim_end_matches('\0')
            .to_owned();
        if !matches!(filesystem.as_str(), "NTFS" | "ReFS") {
            return Ok(None);
        }
        let mut information = BasicInformation::default();
        // FileBasicInfo is read from the checked live handle; no cached path can substitute another file.
        // 从已核验的活动句柄读取 FileBasicInfo，缓存路径不能替换为另一个文件对象。
        if unsafe {
            GetFileInformationByHandleEx(
                file.as_raw_handle(),
                0,
                (&mut information as *mut BasicInformation).cast(),
                std::mem::size_of::<BasicInformation>() as u32,
            )
        } == 0
        {
            let error = std::io::Error::last_os_error();
            return if matches!(error.raw_os_error(), Some(1 | 50 | 87)) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        Ok((information.change_time > 0).then_some(information.change_time))
    }
    #[cfg(not(windows))]
    {
        let _ = file;
        Ok(None)
    }
}
