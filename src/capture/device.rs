use crate::error::AppError;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
    D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;

pub struct D3dDevice {
    pub d3d_device: ID3D11Device,
    pub d3d_context: ID3D11DeviceContext,
}

impl D3dDevice {
    pub fn new() -> Result<Self, AppError> {
        let mut d3d_device = None;
        let mut d3d_context = None;
        unsafe {
            D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_HARDWARE,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None,
                D3D11_SDK_VERSION,
                Some(&mut d3d_device),
                None,
                Some(&mut d3d_context),
            )
            .map_err(|e| AppError::Capture(format!("D3D11CreateDevice failed: {e}")))?;
        }
        Ok(Self {
            d3d_device: d3d_device.unwrap(),
            d3d_context: d3d_context.unwrap(),
        })
    }

    pub fn dxgi_device(&self) -> Result<IDXGIDevice, AppError> {
        self.d3d_device
            .cast::<IDXGIDevice>()
            .map_err(|e| AppError::Capture(format!("cast to IDXGIDevice failed: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d3d_device_creates_successfully() {
        let dev = D3dDevice::new();
        assert!(dev.is_ok(), "D3D11 device creation failed: {:?}", dev.err());
    }
}
