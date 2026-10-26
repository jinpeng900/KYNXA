use std::fs::File;

#[derive(PartialEq, Eq)]
pub struct HandleIdentity {
    pub volume: u64,
    pub identity: u64,
    pub links: u64,
    pub is_link: bool,
}

/// Check the opened object with shared link and file-identity rules.
/// 按统一规则核对已打开对象的链接和文件身份。
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
        // Keep the BY_HANDLE_FILE_INFORMATION buffer valid for the borrowed handle call.
        // 借用句柄调用期间，BY_HANDLE_FILE_INFORMATION 缓冲保持有效。
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

/// Windows change stamps detect edits despite restored write times; otherwise hash the bytes.
/// Windows 变更时间可识别写入时间被还原的修改；不支持时核对文件哈希。
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
        // Coarse timestamps cannot prove unchanged content; hash the full file.
        // 粗粒度时间戳不足以证明内容未变，仍核对完整哈希。
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
        // Read FileBasicInfo from the checked handle, not a cached path.
        // 从已核对的句柄读取 FileBasicInfo，避免缓存路径指向其他文件。
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
